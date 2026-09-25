//! Archive extraction for downloaded mods and manually added files.
//!
//! zip and 7z are handled in-process. rar has no pure-Rust reader, so we shell
//! out to `7z`/`7za`/`unar`/`unrar` when one of them is on PATH — the same
//! formats the WPF build accepted via SevenZipExtractor.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

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

fn extract_rar_external(archive: &Path, dest: &Path) -> Result<()> {
    let candidates: [(&str, Vec<String>); 4] = [
        ("7z", vec!["x".into(), "-y".into(), format!("-o{}", dest.display())]),
        ("7za", vec!["x".into(), "-y".into(), format!("-o{}", dest.display())]),
        ("unar", vec!["-f".into(), "-o".into(), dest.display().to_string()]),
        ("unrar", vec!["x".into(), "-y".into()]),
    ];

    for (program, args) in candidates {
        let mut cmd = std::process::Command::new(program);
        cmd.args(&args).arg(archive);
        if program == "unrar" {
            cmd.arg(dest);
        }
        cmd.current_dir(dest);
        match cmd.status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(_) => continue,
            // Not installed; try the next one.
            Err(_) => continue,
        }
    }

    bail!("RAR archives need 7z, unar or unrar on PATH; none was found")
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
}
