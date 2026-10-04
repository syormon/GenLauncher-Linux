//! Archive extraction for downloaded mods and manually added files.
//!
//! zip and 7z are handled in-process. rar has no pure-Rust reader, so we shell
//! out to 7-Zip, `unrar` or `unar` when one of them is installed — the same
//! formats the WPF build accepted via SevenZipExtractor.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::fs::{self as gfs, extension_of};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    SevenZ,
    Rar,
}

pub fn detect_format(path: &Path) -> Option<Format> {
    match extension_of(path).as_str() {
        "zip" => Some(Format::Zip),
        "7z" => Some(Format::SevenZ),
        "rar" => Some(Format::Rar),
        _ => None,
    }
}

/// True when `path` looks like an archive we can unpack.
pub fn is_supported_archive(path: &Path) -> bool {
    detect_format(path).is_some()
}

/// Unpack `archive` into `dest`, preserving the archive's directory structure.
///
/// When `big_to_gib` is set, every extracted `*.big` is renamed to `*.gib` —
/// the launcher keeps mod archives under that extension so the game does not
/// pick them up until they are linked in.
pub fn extract(archive: &Path, dest: &Path, big_to_gib: bool) -> Result<Vec<PathBuf>> {
    let format = detect_format(archive)
        .with_context(|| format!("unsupported archive type: {}", archive.display()))?;

    fs::create_dir_all(dest)?;

    // Unpack to a sibling staging directory first so a failure cannot leave
    // half-renamed files in the destination.
    let staging = dest.join(".gl-extract-tmp");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;

    let result = match format {
            Format::Zip => extract_zip(archive, &staging),
            Format::SevenZ => sevenz_rust2::decompress_file(archive, &staging)
                .map_err(|e| anyhow::anyhow!("7z extraction failed: {e}")),
            Format::Rar => extract_rar_external(archive, &staging),
        };

    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    let mut written = Vec::new();
    move_tree(&staging, dest, big_to_gib, &mut written)?;
    let _ = fs::remove_dir_all(&staging);

    Ok(written)
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let file = fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // `enclosed_name` rejects `..` and absolute paths (zip-slip).
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut writer = fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut writer)?;
    }
    Ok(())
}

/// A program that can unpack RAR archives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RarTool {
    pub program: PathBuf,
    kind: RarToolKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RarToolKind {
    SevenZip,
    Unrar,
    Unar,
}

/// Every RAR-capable tool on this machine, best first: on PATH, then (on
/// Windows) in the folders the 7-Zip and WinRAR installers use, since neither
/// installer puts itself on PATH.
///
/// `7za` and `7zr` are left out on purpose: those reduced builds cannot read RAR.
pub fn rar_tools() -> Vec<RarTool> {
    let mut tools = Vec::new();
    let mut add = |program: PathBuf, kind| {
        if !tools.iter().any(|t: &RarTool| t.program == program) {
            tools.push(RarTool { program, kind });
        }
    };

    for (name, kind) in [
        ("7z", RarToolKind::SevenZip),
        ("7zz", RarToolKind::SevenZip),
        ("unrar", RarToolKind::Unrar),
        ("unar", RarToolKind::Unar),
    ] {
        if let Some(program) = find_on_path(name) {
            add(program, kind);
        }
    }

    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            let Some(base) = std::env::var_os(var).map(PathBuf::from) else { continue };
            for (rel, kind) in [
                ("7-Zip/7z.exe", RarToolKind::SevenZip),
                ("WinRAR/UnRAR.exe", RarToolKind::Unrar),
            ] {
                let program = base.join(rel);
                if program.is_file() {
                    add(program, kind);
                }
            }
        }
    }

    tools
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(&file)).find(|p| p.is_file())
}

/// True when `path` can be unpacked on this machine right now. zip and 7z
/// always can; rar needs one of [`rar_tools`].
pub fn can_extract(path: &Path) -> bool {
    match detect_format(path) {
        Some(Format::Rar) => !rar_tools().is_empty(),
        Some(_) => true,
        None => false,
    }
}

impl RarTool {
    fn command(&self, archive: &Path, dest: &Path) -> Command {
        let mut cmd = Command::new(&self.program);
        match self.kind {
            RarToolKind::SevenZip => {
                cmd.arg("x").arg("-y").arg(format!("-o{}", dest.display())).arg(archive);
            }
            RarToolKind::Unrar => {
                // unrar takes the destination as a trailing folder argument.
                cmd.arg("x").arg("-y").arg(archive).arg(dest.join(""));
            }
            RarToolKind::Unar => {
                // -D: do not wrap the contents in a folder named after the archive.
                cmd.arg("-f").arg("-D").arg("-o").arg(dest).arg(archive);
            }
        }
        cmd.stdin(std::process::Stdio::null());

        // The launcher is a GUI program; do not flash a console for the tool.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }
}

fn extract_rar_external(archive: &Path, dest: &Path) -> Result<()> {
    let tools = rar_tools();
    if tools.is_empty() {
        bail!("RAR archives need 7-Zip or UnRAR to be installed; neither was found");
    }

    let mut failures = Vec::new();
    for tool in &tools {
        match tool.command(archive, dest).output() {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => {
                // A 7-Zip without its RAR codec (Linux `p7zip` without
                // `p7zip-rar`) fails here; the next tool may still work.
                let text = String::from_utf8_lossy(&output.stderr);
                let reason = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
                failures.push(format!("{}: {}", tool.program.display(), reason.trim()));
            }
            Err(e) => failures.push(format!("{}: {e}", tool.program.display())),
        }
    }

    bail!("could not unpack the RAR archive ({})", failures.join("; "))
}

/// Move everything from `from` into `to`, applying the .big to .gib rename.
fn move_tree(from: &Path, to: &Path, big_to_gib: bool, written: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let src = entry.path();

        if entry.file_type()?.is_dir() {
            let sub = to.join(&name);
            fs::create_dir_all(&sub)?;
            move_tree(&src, &sub, big_to_gib, written)?;
            continue;
        }

        let mut target = to.join(&name);
        if big_to_gib && extension_of(&src) == "big" {
            target = gfs::change_extension(&target, "gib");
        }
        if target.exists() {
            let _ = fs::remove_file(&target);
        }
        gfs::move_file(&src, &target)?;
        written.push(target);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_formats() {
        assert_eq!(detect_format(Path::new("a/ROTR.7z")), Some(Format::SevenZ));
        assert_eq!(detect_format(Path::new("a/ROTR.ZIP")), Some(Format::Zip));
        assert_eq!(detect_format(Path::new("a/ROTR.big")), None);
    }

    #[test]
    fn zip_and_7z_never_need_an_external_tool() {
        assert!(can_extract(Path::new("mod.zip")));
        assert!(can_extract(Path::new("mod.7z")));
        assert!(!can_extract(Path::new("mod.big")));
        // rar depends on what is installed, which is exactly what it reports.
        assert_eq!(can_extract(Path::new("mod.rar")), !rar_tools().is_empty());
    }
}
