//! Static launcher configuration — the Rust counterpart of `EntryPoint`'s const block.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const LAUNCHER_FOLDER: &str = ".GenLauncherFolder";
pub const CONFIG_NAME: &str = ".GenLauncherFolder/GenLauncherCfg.yaml";
pub const MODS_FOLDER: &str = "GLM";
pub const MODS_FOLDER_OLD: &str = "GenLauncherModifications";
pub const LAUNCHER_IMAGE_SUBFOLDER: &str = "LauncherImages";
/// The launcher's own version, taken straight from `Cargo.toml` so the binary,
/// the "Version:" line in the UI and the git tag a release is cut from can
/// never disagree. Bump it in `Cargo.toml` and nowhere else.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const STEAM_FOLDER_NAME: &str = "ZH_Generals";
pub const ORIGINAL_GAME_ALIAS: &str = "Original Game";

pub const ZH_REPOS: &str =
    "https://raw.githubusercontent.com/p0ls3r/GenLauncherModsData/master/ReposModificationDataZH4.yaml";
pub const GEN_REPOS: &str =
    "https://raw.githubusercontent.com/p0ls3r/GenLauncherModsData/master/ReposModificationDataGenerals3.yaml";
pub const ADDONS_FOLDER_NAME: &str = "Addons";
pub const PATCHES_FOLDER_NAME: &str = "Patches";
pub const EXECUTABLES_FOLDER_NAME: &str = "Executables";
pub const VULKAN_DLLS_FOLDER_NAME: &str = "Vulkan";

/// Suffix applied to a game file that a modification temporarily shadows.
pub const REPLACE_SUFFIX: &str = ".GLR";
/// Suffix marking a half-written version folder; these are purged on start/exit.
pub const VERSION_FOLDER_COPY_SUFFIX: &str = ".GLTC";
/// Suffix for the pristine copy of a .big we patch (camera height).
pub const ORIGINAL_FILE_SUFFIX: &str = ".GOF";

pub const GENLAUNCHER_DISCORD: &str = "https://discord.gg/fFGpudz5hV";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Game {
    ZeroHour,
    Generals,
}

impl Game {
    pub fn repos_url(self) -> &'static str {
        match self {
            Game::ZeroHour => ZH_REPOS,
            Game::Generals => GEN_REPOS,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Game::ZeroHour => "Generals Zero Hour",
            Game::Generals => "Generals",
        }
    }

    /// Name of the per-user data directory the game itself writes to.
    pub fn user_data_dir_name(self) -> &'static str {
        match self {
            Game::ZeroHour => "Command and Conquer Generals Zero Hour Data",
            Game::Generals => "Command and Conquer Generals Data",
        }
    }
}

/// Per-run facts established once during start-up.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub connected: bool,
    pub game_mode: Game,
    /// Lower-cased names of the stock .big archives that ship with the game.
    pub game_files: std::collections::HashSet<String>,
}

static GAME_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The game folder the launcher operates on. Fixed for the lifetime of the process
/// so worker threads never depend on the process-wide current directory.
pub fn game_dir() -> &'static Path {
    GAME_DIR.get_or_init(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub fn set_game_dir(path: PathBuf) {
    let _ = GAME_DIR.set(path);
}

/// Resolve a launcher-relative path against the game folder.
pub fn game_path(rel: impl AsRef<Path>) -> PathBuf {
    game_dir().join(rel)
}

/// Directory holding the user's Options.ini, Replays and Maps for `game`.
pub fn user_data_dir(game: Game) -> PathBuf {
    let docs = dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    docs.join(game.user_data_dir_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reported_version_comes_from_cargo_toml() {
        // The release workflow tags from the Cargo.toml version, so the value
        // the launcher shows and self-update compares against must be the same.
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        assert!(!VERSION.is_empty());
        assert!(
            VERSION.split('.').all(|part| part.chars().all(|c| c.is_ascii_alphanumeric())),
            "unexpected version shape: {VERSION}"
        );
    }
}

