//! Cross-platform symbolic-link and path helpers.
//!
//! The launcher's whole approach rests on linking a mod's files into the game
//! folder before launch and removing the links afterwards, so these primitives
//! have to behave identically on Windows and Linux.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Create a symbolic link at `link` pointing at the file `target`.
pub fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
    if let Some(parent) = link.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
}

/// Create a symbolic link at `link` pointing at the directory `target`.
pub fn symlink_dir(target: &Path, link: &Path) -> io::Result<()> {
    if let Some(parent) = link.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
}

/// True when `path` itself is a symlink (never follows it).
pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// Remove a symlink, whichever kind it is. A no-op if `path` is not a link.
pub fn remove_symlink(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_symlink() {
        return Ok(());
    }
    // On Windows a directory symlink must be removed with remove_dir.
    #[cfg(windows)]
    {
        if fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false) {
            return fs::remove_dir(path);
        }
        fs::remove_file(path)
    }
    #[cfg(unix)]
    {
        fs::remove_file(path)
    }
}

/// Verify the filesystem supports symlinks by creating and deleting one.
pub fn can_create_symlinks(dir: &Path) -> bool {
    let original = dir.join("GenLauncherTestFile.test");
    let link = dir.join("GenLauncherTestSymbFile.test");

    let _ = fs::remove_file(&link);
    if fs::write(&original, b"").is_err() {
        return false;
    }

    let ok = symlink_file(&original, &link).is_ok() && fs::symlink_metadata(&link).is_ok();

    let _ = remove_symlink(&link);
    let _ = fs::remove_file(&link);
    let _ = fs::remove_file(&original);
    ok
}

/// Replace `path`'s extension, mirroring .NET's `Path.ChangeExtension`.
pub fn change_extension(path: &Path, ext: &str) -> PathBuf {
    let mut p = path.to_path_buf();
    p.set_extension(ext.trim_start_matches('.'));
    p
}

/// Lower-cased extension without the dot, or "" when there is none.
pub fn extension_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn file_name_of(path: &Path) -> String {
    path.file_name().and_then(|e| e.to_str()).unwrap_or("").to_owned()
}

/// True when the directory holds at least one file, at any depth.
pub fn folder_contains_files(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else { return false };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => subdirs.push(entry.path()),
            Ok(_) => return true,
            Err(_) => {}
        }
    }
    subdirs.iter().any(|d| folder_contains_files(d))
}

/// Move a file, falling back to copy+delete when the rename crosses devices.
pub fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(from, to)?;
            fs::remove_file(from)
        }
    }
}

/// Move a directory, falling back to a recursive copy across devices.
pub fn move_dir(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_dir_recursive(from, to)?;
    fs::remove_dir_all(from)
}

pub fn copy_dir_recursive(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Walk `dir` depth-first, invoking `visit` for every regular file (and link).
pub fn visit_files(dir: &Path, visit: &mut impl FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // symlink_metadata so a link to a directory is treated as a link, not a dir.
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => subdirs.push(path),
            Ok(_) => visit(&path),
            Err(_) => {}
        }
    }
    for sub in subdirs {
        visit_files(&sub, visit);
    }
}

/// Walk `dir` depth-first, invoking `visit` for every directory entry.
pub fn visit_dirs(dir: &Path, visit: &mut impl FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if fs::symlink_metadata(&path).map(|m| m.is_dir()).unwrap_or(false) {
            subdirs.push(path);
        }
    }
    for sub in &subdirs {
        visit(sub);
    }
    for sub in subdirs {
        // A link we just removed no longer exists; read_dir simply yields nothing.
        visit_dirs(&sub, visit);
    }
}

/// Strip characters .NET rejects in file names, so mod names can become folders.
pub fn sanitize_file_name(name: &str) -> String {
    name.chars().filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') && !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_extension_matches_dotnet() {
        assert_eq!(change_extension(Path::new("a/b.gib"), "big"), PathBuf::from("a/b.big"));
        assert_eq!(change_extension(Path::new("a/b"), "big"), PathBuf::from("a/b.big"));
    }

    #[test]
    fn sanitizes_names_for_folders() {
        assert_eq!(sanitize_file_name("Do you like GenLauncher?"), "Do you like GenLauncher");
    }
}
