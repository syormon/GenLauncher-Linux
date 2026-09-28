//! Finding a Steam copy of the game and the Proton setup Steam runs it with.
//!
//! Steam keeps each game's Wine prefix in `steamapps/compatdata/<appid>/pfx`
//! and starts it through a Proton build, usually inside the Steam Linux
//! Runtime container. Launching the same way keeps the game on the prefix,
//! saves and `Options.ini` that Steam itself uses.
#![cfg_attr(windows, allow(dead_code))]

use std::path::{Path, PathBuf};

/// A game installed in a Steam library, with what Proton needs to run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamGame {
    pub app_id: String,
    /// The library's `steamapps` folder.
    pub steamapps: PathBuf,
    /// `STEAM_COMPAT_DATA_PATH`: holds the prefix in `pfx/`.
    pub compat_data: PathBuf,
}

impl SteamGame {
    /// The game in `game_dir`, if that is `<library>/steamapps/common/<installdir>`
    /// and the library has a manifest for it.
    pub fn detect(game_dir: &Path) -> Option<Self> {
        let (steamapps, install_dir) = game_dir.ancestors().find_map(|dir| {
            let common = dir.parent()?;
            let steamapps = common.parent()?;
            let install_dir = dir.file_name()?.to_string_lossy().into_owned();
            (common.file_name()? == "common" && steamapps.file_name()? == "steamapps")
                .then(|| (steamapps.to_path_buf(), install_dir))
        })?;

        let app_id = std::fs::read_dir(&steamapps)
            .ok()?
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let id = name.strip_prefix("appmanifest_")?.strip_suffix(".acf")?.to_owned();
                let manifest = std::fs::read_to_string(e.path()).ok()?;
                vdf_value(&manifest, "installdir")
                    .is_some_and(|dir| dir.eq_ignore_ascii_case(&install_dir))
                    .then_some(id)
            })
            .next()?;

        let compat_data = steamapps.join("compatdata").join(&app_id);
        Some(Self { app_id, steamapps, compat_data })
    }

    pub fn prefix(&self) -> PathBuf {
        self.compat_data.join("pfx")
    }

    /// `STEAM_COMPAT_CLIENT_INSTALL_PATH`: the Steam client that owns this
    /// library. With both a native and a Flatpak Steam, the one whose library
    /// list names ours; otherwise the first client found.
    pub fn client_dir(&self) -> Option<PathBuf> {
        let library = self.steamapps.parent()?;
        let clients = steam_clients();
        clients
            .iter()
            .find(|client| library_paths(client).iter().any(|p| same_dir(p, library)))
            .or_else(|| clients.first())
            .cloned()
    }

    /// The Proton this prefix was last run with (Steam writes its path into
    /// `config_info`), else the newest Proton installed in any library.
    pub fn proton(&self) -> Option<PathBuf> {
        std::fs::read_to_string(self.compat_data.join("config_info"))
            .ok()
            .and_then(|info| proton_from_config_info(&info))
            .filter(|dir| dir.join("proton").is_file())
            .or_else(|| newest_proton(&self.libraries()))
    }

    /// The Steam Linux Runtime entry point `proton_dir` asks to run inside,
    /// if it is installed. Proton 8+ expects this container.
    pub fn runtime_entry_point(&self, proton_dir: &Path) -> Option<PathBuf> {
        let manifest = std::fs::read_to_string(proton_dir.join("toolmanifest.vdf")).ok()?;
        let tool_id = vdf_value(&manifest, "require_tool_appid")?;

        self.libraries().iter().find_map(|steamapps| {
            let acf = std::fs::read_to_string(steamapps.join(format!("appmanifest_{tool_id}.acf"))).ok()?;
            let entry = steamapps.join("common").join(vdf_value(&acf, "installdir")?).join("_v2-entry-point");
            entry.is_file().then_some(entry)
        })
    }

    /// Every library's `steamapps` known to the owning client, ours first.
    fn libraries(&self) -> Vec<PathBuf> {
        let mut libraries = vec![self.steamapps.clone()];
        for client in self.client_dir().into_iter().chain(steam_clients()) {
            for path in library_paths(&client) {
                let steamapps = path.join("steamapps");
                if steamapps.is_dir() && !libraries.iter().any(|l| same_dir(l, &steamapps)) {
                    libraries.push(steamapps);
                }
            }
        }
        libraries
    }
}

/// Every installed game folder in every Steam library, for the caller to pick
/// a Generals or Zero Hour install from when started somewhere else.
pub fn installed_game_dirs() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for client in steam_clients() {
        for library in library_paths(&client) {
            let Ok(entries) = std::fs::read_dir(library.join("steamapps").join("common")) else {
                continue;
            };
            for dir in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
                if !found.iter().any(|f| same_dir(f, &dir)) {
                    found.push(dir);
                }
            }
        }
    }
    found
}

/// Steam client folders on this machine: native first, then Flatpak.
fn steam_clients() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let candidates = [
        home.join(".steam/root"),
        home.join(".steam/steam"),
        home.join(".local/share/Steam"),
        home.join(".steam/debian-installation"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        home.join("snap/steam/common/.local/share/Steam"),
    ];

    let mut clients: Vec<PathBuf> = Vec::new();
    for dir in candidates {
        let Ok(dir) = dir.canonicalize() else { continue };
        let is_client = dir.join("steamapps").is_dir()
            && (dir.join("ubuntu12_32").is_dir() || dir.join("steam.sh").is_file());
        if is_client && !clients.contains(&dir) {
            clients.push(dir);
        }
    }
    clients
}

/// The library folders a Steam client lists in `libraryfolders.vdf`.
fn library_paths(client: &Path) -> Vec<PathBuf> {
    let mut paths = vec![client.to_path_buf()];
    let text = std::fs::read_to_string(client.join("steamapps/libraryfolders.vdf"))
        .or_else(|_| std::fs::read_to_string(client.join("config/libraryfolders.vdf")))
        .unwrap_or_default();
    for line in text.lines() {
        if let Some(("path", value)) = vdf_pair(line) {
            let path = PathBuf::from(value);
            if !paths.iter().any(|p| same_dir(p, &path)) {
                paths.push(path);
            }
        }
    }
    paths
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// `config_info` lists files inside the Proton it was made with, e.g.
/// `/…/common/Proton - Experimental/files/share/fonts/`.
fn proton_from_config_info(info: &str) -> Option<PathBuf> {
    info.lines().find_map(|line| {
        let (dir, _) = line.trim().split_once("/files/")?;
        Some(PathBuf::from(dir))
    })
}

/// Proton builds in `libraries`' `common` folders: the highest numbered one,
/// with Experimental as the last resort.
fn newest_proton(libraries: &[PathBuf]) -> Option<PathBuf> {
    let mut protons: Vec<PathBuf> = libraries
        .iter()
        .filter_map(|l| std::fs::read_dir(l.join("common")).ok())
        .flat_map(|entries| entries.flatten())
        .map(|e| e.path())
        .filter(|p| dir_name(p).starts_with("Proton") && p.join("proton").is_file())
        .collect();
    protons.sort_by_key(|p| proton_rank(&dir_name(p)));
    protons.pop()
}

fn dir_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `Proton 10.0` → (1, [10, 0]); anything unnumbered ranks below.
fn proton_rank(name: &str) -> (u8, Vec<u32>) {
    let numbers: Vec<u32> = name
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect();
    (u8::from(!numbers.is_empty()), numbers)
}

/// `"key"  "value"` on one line of a VDF file.
fn vdf_pair(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.split('"').skip(1).step_by(2);
    Some((parts.next()?, parts.next()?))
}

/// The first value stored under `key`, at any depth.
fn vdf_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(vdf_pair)
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.replace(r"\\", r"\"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_a_steam_install_and_its_prefix() {
        let root = std::env::temp_dir().join(format!("gl-steam-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let steamapps = root.join("steamapps");
        let game = steamapps.join("common/Command & Conquer Generals - Zero Hour");
        fs::create_dir_all(game.join("Data")).unwrap();
        fs::write(
            steamapps.join("appmanifest_2732960.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"2732960\"\n\t\"installdir\"\t\t\"Command & Conquer Generals - Zero Hour\"\n}\n",
        )
        .unwrap();
        fs::write(steamapps.join("appmanifest_1.acf"), "\"installdir\" \"Other\"").unwrap();

        let proton = steamapps.join("common/Proton - Experimental");
        fs::create_dir_all(&proton).unwrap();
        fs::write(proton.join("proton"), "").unwrap();
        fs::write(proton.join("toolmanifest.vdf"), "\"manifest\"\n{\n  \"require_tool_appid\" \"4183110\"\n}\n").unwrap();
        let runtime = steamapps.join("common/SteamLinuxRuntime_4");
        fs::create_dir_all(&runtime).unwrap();
        fs::write(runtime.join("_v2-entry-point"), "").unwrap();
        fs::write(steamapps.join("appmanifest_4183110.acf"), "\"installdir\" \"SteamLinuxRuntime_4\"").unwrap();

        let compat = steamapps.join("compatdata/2732960");
        fs::create_dir_all(compat.join("pfx")).unwrap();
        fs::write(compat.join("config_info"), format!("11.0-100\n{}/files/share/fonts/\n", proton.display())).unwrap();

        let steam = SteamGame::detect(&game.join("Data")).expect("a Steam install");
        assert_eq!(steam.app_id, "2732960");
        assert_eq!(steam.prefix(), compat.join("pfx"));
        assert_eq!(steam.proton(), Some(proton.clone()));
        assert_eq!(steam.runtime_entry_point(&proton), Some(runtime.join("_v2-entry-point")));

        assert_eq!(SteamGame::detect(&root), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ranks_numbered_protons_above_experimental() {
        let mut names = vec!["Proton - Experimental", "Proton 10.0", "Proton 9.0 (Beta)", "Proton 8.0"];
        names.sort_by_key(|n| proton_rank(n));
        assert_eq!(names, ["Proton - Experimental", "Proton 8.0", "Proton 9.0 (Beta)", "Proton 10.0"]);
    }

    #[test]
    fn reads_vdf_pairs() {
        assert_eq!(vdf_pair("\t\t\"path\"\t\t\"/mnt/games\""), Some(("path", "/mnt/games")));
        assert_eq!(vdf_pair("{"), None);
        assert_eq!(
            proton_from_config_info("11.0-100\n/s/common/Proton 10.0/files/share/fonts/\n"),
            Some(PathBuf::from("/s/common/Proton 10.0"))
        );
    }
}
