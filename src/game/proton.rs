//! Running the Windows game through Steam's Proton, until a native Linux
//! build exists.
//!
//! The game must be a Steam install. It is started with the same Proton,
//! prefix (`steamapps/compatdata/<appid>/pfx`) and Steam Linux Runtime that
//! Steam uses for it, so saves and `Options.ini` are shared with Steam's own
//! launch. Proton is Wine underneath, which is where the DLL overrides and the
//! prefix's registry come from.
//!
//! Everything here is plain path and string handling, so it compiles and is
//! tested on every platform; only non-Windows builds actually launch through it.
#![cfg_attr(windows, allow(dead_code))]

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::config;
use crate::game::steam::SteamGame;
use crate::model::ProtonSettings;
use crate::util::fs as gfs;

const DLL_OVERRIDES: &str = "WINEDLLOVERRIDES";

/// A Proton build paired with the Steam install it runs.
pub struct Proton {
    /// Proton's `proton` script.
    pub script: PathBuf,
    pub steam: SteamGame,
    /// The Steam Linux Runtime `_v2-entry-point` to run Proton inside.
    runtime: Option<PathBuf>,
    env: Vec<(String, String)>,
}

impl Proton {
    pub fn resolve(settings: &ProtonSettings) -> Result<Self> {
        let steam = SteamGame::detect(config::game_dir()).context(
            "the game must be a Steam install (in a library's steamapps/common) to run through Proton",
        )?;
        let dir = proton_dir(&settings.proton, &steam).with_context(|| {
            if settings.proton.trim().is_empty() {
                "no Proton found. Install one in Steam (Library → Tools) and start the game once from Steam".to_owned()
            } else {
                format!("`{}` is not a Proton folder", settings.proton.trim())
            }
        })?;
        let runtime = steam.runtime_entry_point(&dir);
        Ok(Self { script: dir.join("proton"), steam, runtime, env: parse_env(&settings.env) })
    }

    /// The Wine prefix the game runs in.
    pub fn prefix(&self) -> PathBuf {
        self.steam.prefix()
    }

    /// A command that runs the Windows program `exe` from `cwd`, and whose
    /// process lasts until the game has exited. With `desktop`, the program
    /// runs inside a Wine virtual desktop of that size.
    pub fn command(&self, exe: &Path, cwd: &Path, desktop: Option<(u32, u32)>) -> Command {
        let mut cmd = self.base_command("waitforexitandrun", cwd);
        match desktop {
            Some((width, height)) => {
                cmd.arg(r"C:\windows\explorer.exe")
                    .arg(format!("/desktop=GenLauncher,{width}x{height}"))
                    .arg(windows_path(exe));
            }
            None => {
                cmd.arg(exe);
            }
        }
        cmd.current_dir(cwd);

        // Wine's debug channels are noisy and cost frames; keep an explicit choice.
        if std::env::var_os("WINEDEBUG").is_none() {
            cmd.env("WINEDEBUG", "-all");
        }

        // DLLs next to the game (GenTool's d3d8.dll, the Vulkan layer) must win
        // over Wine's built-in copies, as they would on Windows.
        let mut dirs = vec![cwd];
        dirs.extend(exe.parent());
        let overrides = merge_overrides(&[
            native_dll_overrides(&dirs),
            std::env::var(DLL_OVERRIDES).ok(),
            self.env.iter().find(|(k, _)| k == DLL_OVERRIDES).map(|(_, v)| v.clone()),
        ]);
        if let Some(overrides) = overrides {
            cmd.env(DLL_OVERRIDES, overrides);
        }

        // The user's own variables go last so they can override our defaults.
        for (key, value) in self.env.iter().filter(|(k, _)| k != DLL_OVERRIDES) {
            cmd.env(key, value);
        }
        cmd
    }

    /// `[runtime --verb=… --] proton <verb>`, with the environment Steam would
    /// give it. The program to run follows as the next argument.
    fn base_command(&self, verb: &str, install_dir: &Path) -> Command {
        let mut cmd = match &self.runtime {
            Some(entry) => {
                let mut cmd = Command::new(entry);
                cmd.arg(format!("--verb={verb}")).arg("--").arg(&self.script);
                cmd
            }
            None => Command::new(&self.script),
        };
        cmd.arg(verb);

        let steam = &self.steam;
        cmd.env("STEAM_COMPAT_DATA_PATH", &steam.compat_data)
            .env("STEAM_COMPAT_INSTALL_PATH", install_dir)
            .env("STEAM_COMPAT_APP_ID", &steam.app_id)
            .env("SteamAppId", &steam.app_id)
            .env("SteamGameId", &steam.app_id);
        if let Some(client) = steam.client_dir() {
            cmd.env("STEAM_COMPAT_CLIENT_INSTALL_PATH", client);
        }
        // Shared into the runtime container; Proton also maps it as drive S:.
        cmd.env("STEAM_COMPAT_LIBRARY_PATHS", &steam.steamapps);
        if let Some(tool) = self.script.parent() {
            cmd.env("STEAM_COMPAT_TOOL_PATHS", tool);
        }
        cmd
    }

    /// The Proton build's folder name, e.g. `Proton - Experimental`.
    pub fn name(&self) -> String {
        self.script
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.script.display().to_string())
    }
}

/// One line describing what a launch would use, for the options screen.
pub fn describe(settings: &ProtonSettings) -> Result<String> {
    let proton = Proton::resolve(settings)?;
    let runtime = if proton.runtime.is_some() { ", in the Steam Linux Runtime" } else { "" };
    Ok(format!(
        "{} (Steam app {}{runtime}) · prefix {}",
        proton.name(),
        proton.steam.app_id,
        proton.prefix().display()
    ))
}

/// The prefix a launch would use, if the game can run through Proton at all.
pub fn prefix(settings: &ProtonSettings) -> Option<PathBuf> {
    Proton::resolve(settings).ok().map(|p| p.prefix())
}

/// Open `winecfg` for the game's prefix without waiting for it.
pub fn open_winecfg(settings: &ProtonSettings) -> Result<()> {
    let proton = Proton::resolve(settings)?;
    let mut cmd = proton.base_command("run", config::game_dir());
    cmd.arg("winecfg");
    for (key, value) in &proton.env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().context("cannot start winecfg")?;
    // Reap it when it closes so it does not linger as a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

// ---------------------------------------------------------------------------
// Locating Proton
// ---------------------------------------------------------------------------

/// The Proton to use: the configured one (its folder or its `proton`
/// script), or, with nothing configured, the one Steam runs this game with.
fn proton_dir(configured: &str, steam: &SteamGame) -> Option<PathBuf> {
    let configured = configured.trim();
    if configured.is_empty() {
        return steam.proton();
    }
    let path = expand_home(configured);
    let dir = if path.file_name().is_some_and(|n| n == "proton") { path.parent()?.to_path_buf() } else { path };
    dir.join("proton").is_file().then_some(dir)
}

/// `/games/zh/generals.exe` → `Z:\games\zh\generals.exe`. Wine maps `Z:` to
/// the Linux root, so this names the same file from inside the prefix.
fn windows_path(path: &Path) -> String {
    format!("Z:{}", path.to_string_lossy().replace('/', "\\"))
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            dirs::home_dir().unwrap_or_default().join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(path),
    }
}

/// Whitespace-separated `KEY=VALUE` pairs. Anything else is ignored.
pub fn parse_env(text: &str) -> Vec<(String, String)> {
    text.split_whitespace()
        .filter_map(|pair| pair.split_once('='))
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// The prefix's Documents folder
// ---------------------------------------------------------------------------

/// The Windows "My Documents" of `prefix`, where the game keeps Options.ini,
/// replays and maps. Read from the registry first, since Wine may point it
/// anywhere (by default it links to the Linux user's `~/Documents`).
pub fn documents_dir(prefix: &Path) -> Option<PathBuf> {
    let from_registry = std::fs::read(prefix.join("user.reg"))
        .ok()
        .and_then(|bytes| shell_folder(&String::from_utf8_lossy(&bytes), "Personal"))
        .and_then(|windows_path| windows_to_unix(prefix, &windows_path))
        .filter(|p| p.is_dir());
    if from_registry.is_some() {
        return from_registry;
    }

    let users = prefix.join("drive_c").join("users");
    // Proton prefixes use `steamuser`; otherwise any non-shared profile.
    let mut names = vec!["steamuser".to_owned()];
    if let Ok(entries) = std::fs::read_dir(&users) {
        names.extend(
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.eq_ignore_ascii_case("public")),
        );
    }

    names.iter().find_map(|name| {
        ["Documents", "My Documents"]
            .iter()
            .map(|docs| gfs::resolve_case_insensitive(&users, &Path::new(name).join(docs)))
            .find(|p| p.is_dir())
    })
}

/// The Documents folder the game uses in `prefix`. Before the first launch
/// the prefix does not exist yet; this is then the folder Proton will create
/// (`steamuser`'s), so an `Options.ini` written there ahead of time is kept.
/// Without one the game crashes on its first start.
pub fn game_documents_dir(prefix: &Path) -> PathBuf {
    documents_dir(prefix).unwrap_or_else(|| prefix.join("drive_c/users/steamuser/Documents"))
}

/// A value from `HKCU\...\Explorer\Shell Folders` in a Wine `user.reg`.
fn shell_folder(user_reg: &str, name: &str) -> Option<String> {
    let section = r"[software\\microsoft\\windows\\currentversion\\explorer\\shell folders]";
    let key = format!("\"{name}\"=");
    let mut in_section = false;

    for line in user_reg.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line.to_ascii_lowercase().starts_with(section);
            continue;
        }
        if in_section {
            if let Some(value) = line.strip_prefix(&key) {
                return Some(value.trim().trim_matches('"').replace(r"\\", r"\"));
            }
        }
    }
    None
}

/// `C:\users\me\Documents` → `<prefix>/dosdevices/c:/users/me/Documents`.
fn windows_to_unix(prefix: &Path, windows_path: &str) -> Option<PathBuf> {
    let (drive, rest) = windows_path.split_once(':')?;
    let drive = drive.to_ascii_lowercase();
    if drive.len() != 1 {
        return None;
    }

    let mut base = prefix.join("dosdevices").join(format!("{drive}:"));
    if !base.exists() && drive == "c" {
        base = prefix.join("drive_c");
    }
    if !base.exists() {
        return None;
    }

    let rel: PathBuf = rest.split('\\').filter(|c| !c.is_empty()).collect();
    Some(gfs::resolve_case_insensitive(&base, &rel))
}

// ---------------------------------------------------------------------------
// DLL overrides
// ---------------------------------------------------------------------------

/// `name1,name2=n,b` for every DLL in `dirs`. Windows loads a DLL from the
/// program's folder before its own; Wine prefers its built-in version of any
/// DLL it implements (d3d8, d3d9, ...) unless told otherwise.
fn native_dll_overrides(dirs: &[&Path]) -> Option<String> {
    let mut names: Vec<String> = dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten())
        .map(|e| e.path())
        .filter(|p| gfs::extension_of(p) == "dll")
        // A dangling link would make Wine fail to load the DLL at all.
        .filter(|p| p.exists())
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()))
        .collect();
    names.sort();
    names.dedup();

    (!names.is_empty()).then(|| format!("{}=n,b", names.join(",")))
}

/// Join override lists; Wine lets a later entry override an earlier one.
fn merge_overrides(parts: &[Option<String>]) -> Option<String> {
    let joined: Vec<&str> =
        parts.iter().flatten().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    (!joined.is_empty()).then(|| joined.join(";"))
}

// ---------------------------------------------------------------------------
// Waiting for the game
// ---------------------------------------------------------------------------

/// Block until no process named like one of `names` is left.
///
/// The retail `generals.exe` is a stub that starts `game.dat` and exits at
/// once, so waiting on the process we spawned is not enough: the mod links
/// would be torn down under a running game. Wine shows Windows programs in
/// `/proc` under their Windows command line, which is what we match on.
pub fn wait_for_processes(names: &[&str]) {
    // Give a stub's child a moment to show up before deciding nothing runs.
    let grace_end = Instant::now() + Duration::from_secs(3);
    let mut seen = false;

    loop {
        let running = any_process_named(names);
        if running {
            seen = true;
        } else if seen || Instant::now() >= grace_end {
            return;
        }
        std::thread::sleep(Duration::from_millis(if running { 1000 } else { 250 }));
    }
}

fn any_process_named(names: &[&str]) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else { return false };

    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
        .any(|cmdline| cmdline_mentions(&cmdline, names))
}

/// True when any of the first few arguments names one of `names`. Checking a
/// few covers both `C:\...\game.dat` and `wine64-preloader wine64 game.dat`.
fn cmdline_mentions(cmdline: &[u8], names: &[&str]) -> bool {
    cmdline
        .split(|b| *b == 0)
        .take(3)
        .map(String::from_utf8_lossy)
        .any(|arg| {
            let base = arg.rsplit(['/', '\\']).next().unwrap_or_default().trim().to_owned();
            names.iter().any(|n| base.eq_ignore_ascii_case(n))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gl-wine-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_my_documents_from_the_registry() {
        let reg = "WINE REGISTRY Version 2\n\
            [Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Explorer\\\\User Shell Folders] 1700000000\n\
            \"Personal\"=str(2):\"%USERPROFILE%\\\\Documents\"\n\
            [Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Explorer\\\\Shell Folders] 1700000000\n\
            #time=1da\n\
            \"Desktop\"=\"C:\\\\users\\\\me\\\\Desktop\"\n\
            \"Personal\"=\"C:\\\\users\\\\me\\\\Documents\"\n";
        assert_eq!(shell_folder(reg, "Personal").as_deref(), Some(r"C:\users\me\Documents"));
        assert_eq!(shell_folder(reg, "Missing"), None);
    }

    #[test]
    fn maps_the_documents_folder_into_the_prefix() {
        let prefix = scratch("docs");
        let docs = prefix.join("drive_c").join("users").join("me").join("Documents");
        fs::create_dir_all(&docs).unwrap();
        fs::write(
            prefix.join("user.reg"),
            "[Software\\\\Microsoft\\\\Windows\\\\CurrentVersion\\\\Explorer\\\\Shell Folders]\n\
             \"Personal\"=\"C:\\\\users\\\\me\\\\Documents\"\n",
        )
        .unwrap();

        assert_eq!(documents_dir(&prefix), Some(docs.clone()));

        // Without the registry entry, the profile folder is still found.
        fs::remove_file(prefix.join("user.reg")).unwrap();
        assert_eq!(documents_dir(&prefix), Some(docs));

        // A prefix Proton has not created yet gets steamuser's Documents.
        let unborn = prefix.join("not-yet");
        assert_eq!(game_documents_dir(&unborn), unborn.join("drive_c/users/steamuser/Documents"));

        let _ = fs::remove_dir_all(&prefix);
    }

    #[test]
    fn overrides_every_dll_beside_the_game() {
        let dir = scratch("dlls");
        fs::write(dir.join("d3d8.dll"), b"").unwrap();
        fs::write(dir.join("BINKW32.DLL"), b"").unwrap();
        fs::write(dir.join("generals.exe"), b"").unwrap();

        assert_eq!(native_dll_overrides(&[&dir]).as_deref(), Some("binkw32,d3d8=n,b"));
        assert_eq!(
            merge_overrides(&[Some("d3d8=n,b".into()), None, Some("dxgi=b".into())]).as_deref(),
            Some("d3d8=n,b;dxgi=b")
        );
        assert_eq!(merge_overrides(&[None, Some("  ".into())]), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_environment_pairs() {
        assert_eq!(
            parse_env("DXVK_HUD=fps  WINEDEBUG=-all\nnonsense =oops"),
            vec![
                ("DXVK_HUD".to_owned(), "fps".to_owned()),
                ("WINEDEBUG".to_owned(), "-all".to_owned()),
            ]
        );
    }

    #[test]
    fn matches_game_processes_by_their_windows_name() {
        let wine_style = b"C:\\Games\\Zero Hour\\game.dat\0-win\0";
        let preloader = b"/usr/bin/wine64-preloader\0/usr/bin/wine64\0Z:\\zh\\GAME.DAT\0";
        let unrelated = b"/usr/bin/bash\0-c\0echo game.dat is here\0";

        assert!(cmdline_mentions(wine_style, &["game.dat"]));
        assert!(cmdline_mentions(preloader, &["game.dat"]));
        assert!(!cmdline_mentions(unrelated, &["game.dat"]));
    }
}
