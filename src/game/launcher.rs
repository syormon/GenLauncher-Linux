//! Prepares the game folder, starts the game, and puts the folder back the way
//! it was. Port of `GameLauncher`.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::config::{self, Game, SessionInfo};
use crate::game::{big, symlinks};
use crate::model::{ModVersion, ModificationType};
use crate::util::fs as gfs;

/// Loose files a modification may ship that must be hidden before launch.
const CUSTOM_FILE_EXTENSIONS: &[&str] =
    &["w3d", "dds", "tga", "ini", "scb", "wnd", "csf", "str"];

/// Extensions whose checksum is worth verifying against the repository.
const EXTENSIONS_TO_CHECK_HASH: &[&str] =
    &["w3d", "big", "bik", "dds", "tga", "ini", "scb", "wnd", "csf", "str", "gib"];

/// Extensions skipped entirely by the integrity check.
pub const EXCEPT_EXTENSIONS: &[&str] = &["exe", "dll"];

/// Scripts the stock game ships that a mod commonly shadows.
const SCRIPT_FILES: &[&str] =
    &["MultiplayerScripts.scb", "Scripts.ini", "SkirmishScripts.scb"];

/// Undo every link and rename the launcher made. Safe to call at any time.
pub fn restore_game_folder() {
    let root = config::game_dir();

    gfs::visit_files(root, &mut |file| {
        symlinks::remove_symlink_file(file);
        remove_replace_suffixes(file);
    });

    gfs::visit_dirs(root, &mut |dir| {
        symlinks::remove_symlink_folder(dir);
    });
}

/// Delete leftover `*.GLTC` staging folders from an interrupted download.
pub fn delete_temp_folders(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if !std::fs::symlink_metadata(&path).map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        if gfs::file_name_of(&path).contains(config::VERSION_FOLDER_COPY_SUFFIX) {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                log::debug!("could not remove staging folder {}: {e}", path.display());
            }
        } else {
            delete_temp_folders(&path);
        }
    }
}

/// Link the selected modifications into the game folder.
pub fn prepare_game_files(
    versions: &[ModVersion],
    session: &SessionInfo,
    camera_height: i32,
    has_selected_mod: bool,
    set_camera_height: bool,
    create_links_on_empty_bigs: bool,
) {
    let root = config::game_dir();

    gfs::visit_files(root, &mut |file| {
        rename_non_game_big_file(file, &session.game_files);
        rename_custom_file(file);
        symlinks::remove_symlink_file(file);
    });

    if !has_selected_mod {
        restore_stock_scripts();
    }

    if set_camera_height && session.game_mode == Game::ZeroHour {
        apply_camera_height(versions, camera_height);
    }

    for version in versions {
        symlinks::create_mirrors_from_folder(
            &version.folder_path(),
            "",
            create_links_on_empty_bigs,
            version.kind() != ModificationType::Executable,
        );
    }

    for exe in versions.iter().filter(|v| v.kind() == ModificationType::Executable) {
        symlinks::create_mirrors_for_exe(root, &exe.folder_name());
    }
}

/// With no mod selected, the stock scripts must come back out of the `.GLR` stash.
fn restore_stock_scripts() {
    let root = config::game_dir();
    for prefix in ["Data/Scripts", &format!("{}/Data/Scripts", config::STEAM_FOLDER_NAME)] {
        for name in SCRIPT_FILES {
            let stashed = root.join(format!("{prefix}/{name}{}", config::REPLACE_SUFFIX));
            remove_file_suffix(&stashed, config::REPLACE_SUFFIX);
        }
    }
}

fn remove_replace_suffixes(file: &Path) {
    let text = file.to_string_lossy();
    if text.contains(config::REPLACE_SUFFIX) {
        remove_file_suffix(file, config::REPLACE_SUFFIX);
    } else if text.contains(config::ORIGINAL_FILE_SUFFIX) {
        remove_file_suffix(file, config::ORIGINAL_FILE_SUFFIX);
    }
}

/// Rename `file` back by stripping `suffix`, replacing whatever is in the way.
fn remove_file_suffix(file: &Path, suffix: &str) {
    if !file.exists() {
        return;
    }
    let restored = PathBuf::from(file.to_string_lossy().replace(suffix, ""));
    if restored == file {
        return;
    }

    if restored.exists() || gfs::is_symlink(&restored) {
        let _ = gfs::remove_symlink(&restored);
        let _ = std::fs::remove_file(&restored);
    }
    if let Err(e) = gfs::move_file(file, &restored) {
        log::debug!("could not restore {}: {e}", restored.display());
    }
}

/// Hide a .big that did not ship with the game, so only linked mods are loaded.
fn rename_non_game_big_file(file: &Path, game_files: &HashSet<String>) {
    let ext = gfs::extension_of(file);
    if !ext.contains("big") {
        return;
    }
    let name = gfs::file_name_of(file).to_ascii_lowercase();
    if game_files.contains(&name) {
        return;
    }
    if file.to_string_lossy().contains(config::STEAM_FOLDER_NAME) {
        return;
    }
    if !big::is_big_archive(file) {
        return;
    }
    shadow_file(file);
}

/// Hide a loose mod file (.ini, .w3d, ...) sitting in the game folder.
fn rename_custom_file(file: &Path) {
    let ext = gfs::extension_of(file);
    if !CUSTOM_FILE_EXTENSIONS.contains(&ext.as_str()) {
        return;
    }
    // Files inside our own mod store are the source of the links, not targets.
    if file.to_string_lossy().contains(config::MODS_FOLDER) {
        return;
    }
    shadow_file(file);
}

/// Move `file` aside under the `.GLR` suffix, or drop it if a stash exists.
fn shadow_file(file: &Path) {
    let shadowed = PathBuf::from(format!("{}{}", file.display(), config::REPLACE_SUFFIX));
    if shadowed.exists() {
        let _ = std::fs::remove_file(file);
    } else if let Err(e) = gfs::move_file(file, &shadowed) {
        log::debug!("could not shadow {}: {e}", file.display());
    }
}

/// Patch the camera cap into whichever archive carries `GameData.ini`.
fn apply_camera_height(versions: &[ModVersion], height: i32) {
    if height == 0 {
        return;
    }

    let mut bigs: Vec<PathBuf> =
        versions.iter().flat_map(|v| big::big_files_in_folder(&v.folder_path())).collect();
    bigs.sort_by_key(|p| gfs::file_name_of(p));

    let Some(target) = bigs.into_iter().find(|p| big::file_contains_game_data_ini(p)) else {
        return;
    };

    let pristine =
        PathBuf::from(format!("{}{}", target.display(), config::ORIGINAL_FILE_SUFFIX));
    if !pristine.exists() {
        if let Err(e) = std::fs::copy(&target, &pristine) {
            log::warn!("could not back up {}: {e}", target.display());
            return;
        }
    }

    if let Err(e) = big::set_camera_height(&target, height) {
        log::warn!("could not set camera height in {}: {e}", target.display());
    }
}

/// Files a repository says a modification should consist of.
#[derive(Debug, Clone)]
pub struct RemoteFileInfo {
    pub file_name: String,
    pub hash: String,
    pub size: u64,
}

/// Compare the installed files against the repository listing.
/// Mirrors `AreModFilesCorrect`: a missing or mismatching file fails the check.
pub fn mod_files_are_correct(files: &[RemoteFileInfo]) -> bool {
    let root = config::game_dir();

    for info in files {
        let declared = root.join(&info.file_name);
        let ext = gfs::extension_of(&declared);
        if ext.is_empty() || EXCEPT_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }

        // The file may have been linked in under its .big name.
        let as_big = gfs::change_extension(&declared, "big");
        let present = if declared.exists() {
            declared
        } else if as_big.exists() {
            as_big
        } else {
            return false;
        };

        let present_ext = gfs::extension_of(&present);
        let needs_hash =
            EXTENSIONS_TO_CHECK_HASH.contains(&present_ext.as_str()) || big::is_big_archive(&present);

        if needs_hash {
            match crate::util::md5_file(&present) {
                Ok(hash) if hash.eq_ignore_ascii_case(&info.hash) => {}
                _ => return false,
            }
        }
    }
    true
}

/// How a game run finished.
pub struct RunOutcome {
    /// True when the session lasted long enough to be a real game.
    pub played_long_enough: bool,
}

/// Start the game executable and block until it exits.
pub fn run_game(
    versions: &[ModVersion],
    windowed: bool,
    quick_start: bool,
    extra_params: &str,
) -> Result<RunOutcome> {
    let executables: Vec<&ModVersion> = versions
        .iter()
        .filter(|v| v.kind() == ModificationType::Executable && !v.info.replaces_original_wb_file)
        .collect();

    let main_exe = executables
        .iter()
        .find(|v| v.info.replaces_original_game_file)
        .map(|v| v.info.executable_file_name.clone())
        .unwrap_or_else(|| "generals.exe".to_owned());

    let mut args = Vec::new();
    if windowed {
        args.push("-win".to_owned());
    }
    if quick_start {
        args.push("-quickstart".to_owned());
        args.push("-noshellmap".to_owned());
    }
    args.extend(
        extra_params.split_whitespace().map(str::to_owned),
    );

    let started = Instant::now();
    let mut child = spawn_exe(&main_exe, &args).with_context(|| format!("cannot start {main_exe}"))?;

    // Companion executables (overlays, online clients) run alongside the game.
    for exe in executables
        .iter()
        .filter(|v| !v.info.replaces_original_game_file && !v.info.executable_file_name.is_empty())
    {
        if let Err(e) = spawn_exe(&exe.info.executable_file_name, &[]) {
            log::warn!("could not start {}: {e}", exe.info.executable_file_name);
        }
    }

    let _ = child.wait();

    Ok(RunOutcome { played_long_enough: started.elapsed().as_secs() >= 12 })
}

/// Start World Builder and block until it exits.
pub fn run_world_builder(versions: &[ModVersion]) -> Result<()> {
    let exe = versions
        .iter()
        .find(|v| {
            v.kind() == ModificationType::Executable && v.info.replaces_original_wb_file
        })
        .map(|v| v.info.executable_file_name.clone())
        .unwrap_or_else(|| "WorldBuilder.exe".to_owned());

    let mut child = spawn_exe(&exe, &[]).with_context(|| format!("cannot start {exe}"))?;
    let _ = child.wait();
    Ok(())
}

/// Launch a Windows executable from the game folder.
///
/// On Windows this is a direct spawn. Elsewhere the game is a Windows binary,
/// so it is handed to Wine when one is available.
fn spawn_exe(exe_name: &str, args: &[String]) -> Result<std::process::Child> {
    let root = config::game_dir();
    let exe_path = root.join(exe_name);

    #[cfg(windows)]
    {
        Ok(Command::new(&exe_path).args(args).current_dir(root).spawn()?)
    }

    #[cfg(not(windows))]
    {
        // A native Linux build of the game would be spawned directly.
        if exe_path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
            != Some("exe".to_owned())
        {
            return Ok(Command::new(&exe_path).args(args).current_dir(root).spawn()?);
        }

        let runner = which_wine().context(
            "this is a Windows game executable and no `wine` was found on PATH",
        )?;
        Ok(Command::new(runner).arg(&exe_path).args(args).current_dir(root).spawn()?)
    }
}

#[cfg(not(windows))]
fn which_wine() -> Option<String> {
    for candidate in ["wine", "wine64"] {
        if Command::new(candidate)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return Some(candidate.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_check_skips_binaries_and_extensionless_files() {
        let files = vec![
            RemoteFileInfo {
                file_name: "does-not-exist.dll".into(),
                hash: "0".into(),
                size: 0,
            },
            RemoteFileInfo { file_name: "no-extension".into(), hash: "0".into(), size: 0 },
        ];
        assert!(mod_files_are_correct(&files));
    }

    #[test]
    fn integrity_check_fails_on_a_missing_data_file() {
        let files = vec![RemoteFileInfo {
            file_name: "definitely/missing.ini".into(),
            hash: "0".into(),
            size: 0,
        }];
        assert!(!mod_files_are_correct(&files));
    }
}
