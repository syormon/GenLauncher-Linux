//! Links a modification's files into the game folder and takes them out again.
//! Port of `SymbolicLinkHandler`.
//!
//! Everything here works on paths relative to the game directory; `target` is
//! always inside the game folder, `source` always inside `GLM/`.

use std::path::{Path, PathBuf};

use crate::config;
use crate::game::big;
use crate::util::fs as gfs;

/// Extensions never linked in for a modification: the game loads its own.
pub const EXCEPT_EXTENSIONS: &[&str] = &["exe", "dll"];

/// A .big of exactly this size is a placeholder that only masks a stock file.
const PLACEHOLDER_BIG_SIZE: u64 = 24;

pub fn remove_symlink_file(path: &Path) {
    if gfs::is_symlink(path) {
        if let Err(e) = gfs::remove_symlink(path) {
            log::debug!("could not unlink {}: {e}", path.display());
        }
    }
}

pub fn remove_symlink_folder(path: &Path) {
    if gfs::is_symlink(path) {
        if let Err(e) = gfs::remove_symlink(path) {
            log::debug!("could not unlink folder {}: {e}", path.display());
        }
    }
}

/// Link every file of a version's folder into the matching place in the game
/// folder. `links_for_mod` excludes executables and libraries.
pub fn create_mirrors_from_folder(
    source_folder: &Path,
    target_rel: &str,
    create_links_on_empty_bigs: bool,
    links_for_mod: bool,
) {
    if !target_rel.is_empty() {
        let target_dir = config::game_path(target_rel);
        if !target_dir.exists() {
            let _ = std::fs::create_dir_all(&target_dir);
        }
    }

    let Ok(entries) = std::fs::read_dir(source_folder) else { return };
    let mut subdirs = Vec::new();

    for entry in entries.flatten() {
        let source = entry.path();
        let Ok(file_type) = entry.file_type() else { continue };

        if file_type.is_dir() {
            subdirs.push(source);
            continue;
        }

        let ext = gfs::extension_of(&source);
        if links_for_mod && EXCEPT_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }

        // A .GOF is our own backup of a patched archive, never a mod file.
        if source.to_string_lossy().contains(config::ORIGINAL_FILE_SUFFIX) {
            continue;
        }

        let name = gfs::file_name_of(&source);
        let target_rel_file =
            if target_rel.is_empty() { name } else { format!("{target_rel}/{name}") };

        let result = if big::is_big_archive(&source) {
            create_mirror_for_big(&source, &target_rel_file, create_links_on_empty_bigs)
        } else {
            create_mirror_for_non_big(&source, &config::game_path(&target_rel_file))
        };

        if let Err(e) = result {
            log::warn!("cannot replace file {target_rel_file}: {e}");
        }
    }

    for dir in subdirs {
        let name = gfs::file_name_of(&dir);
        let nested = if target_rel.is_empty() { name } else { format!("{target_rel}/{name}") };
        create_mirrors_from_folder(&dir, &nested, false, links_for_mod);
    }
}

/// Link a BIG archive over its stock counterpart, stashing the original.
fn create_mirror_for_big(
    source: &Path,
    target_rel: &str,
    create_links_on_empty_bigs: bool,
) -> std::io::Result<()> {
    let target = config::game_path(target_rel);
    let target_big = gfs::change_extension(&target, "big");

    let mut create_link = true;

    if target_big.exists() {
        let shadowed = PathBuf::from(format!(
            "{}{}",
            target_big.display(),
            config::REPLACE_SUFFIX
        ));

        if shadowed.exists() || gfs::is_symlink(&target_big) {
            // We already have the pristine copy stashed, or this is our own link.
            let _ = gfs::remove_symlink(&target_big);
            let _ = std::fs::remove_file(&target_big);
        } else {
            gfs::move_file(&target_big, &shadowed)?;
        }

        // A placeholder archive only needs to hide the stock file, not replace it.
        if let Ok(meta) = std::fs::metadata(source) {
            if meta.len() == PLACEHOLDER_BIG_SIZE && !create_links_on_empty_bigs {
                create_link = false;
            }
        }
    }

    if !create_link {
        return Ok(());
    }

    // A source already carrying our replace suffix links back under its real name.
    let link_at = if source.extension().and_then(|e| e.to_str())
        == Some(config::REPLACE_SUFFIX.trim_start_matches('.'))
    {
        gfs::change_extension(&gfs::change_extension(&target, ""), "big")
    } else {
        target_big
    };

    let _ = std::fs::remove_file(&link_at);
    gfs::symlink_file(source, &link_at)
}

/// Link a plain file, stashing whatever was already there.
pub fn create_mirror_for_non_big(source: &Path, target: &Path) -> std::io::Result<()> {
    if target.exists() || gfs::is_symlink(target) {
        let shadowed =
            PathBuf::from(format!("{}{}", target.display(), config::REPLACE_SUFFIX));

        if gfs::is_symlink(target) {
            let _ = gfs::remove_symlink(target);
        } else {
            if shadowed.exists() {
                let _ = std::fs::remove_file(&shadowed);
            }
            gfs::move_file(target, &shadowed)?;
        }
    }

    gfs::symlink_file(source, target)
}

/// Mirror the whole game folder into a custom executable's folder, so that
/// executable can run with the game's data beside it.
pub fn create_mirrors_for_exe(source_folder: &Path, target_rel: &str) {
    let Ok(entries) = std::fs::read_dir(source_folder) else { return };

    for entry in entries.flatten() {
        let source = entry.path();
        let Ok(file_type) = entry.file_type() else { continue };
        let name = gfs::file_name_of(&source);

        if file_type.is_dir() {
            // The mod store and our own folder must not be mirrored into itself.
            if name == config::MODS_FOLDER || name == config::LAUNCHER_FOLDER {
                continue;
            }
            let link = config::game_path(format!("{target_rel}/{name}"));
            if !link.exists() && !gfs::is_symlink(&link) {
                if let Err(e) = gfs::symlink_dir(&source, &link) {
                    log::debug!("cannot mirror folder {name}: {e}");
                }
            }
            continue;
        }

        if source.to_string_lossy().contains(config::ORIGINAL_FILE_SUFFIX) {
            continue;
        }

        let target_rel_file =
            if target_rel.is_empty() { name } else { format!("{target_rel}/{name}") };
        let target = config::game_path(&target_rel_file);

        // Never overwrite a file the executable brings itself.
        if target.exists() || gfs::is_symlink(&target) {
            continue;
        }
        if let Err(e) = gfs::symlink_file(&source, &target) {
            log::debug!("cannot mirror file {target_rel_file}: {e}");
        }
    }
}
