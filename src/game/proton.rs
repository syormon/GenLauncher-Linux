//! Running the Windows game through Proton, until a native Linux build exists.
//!
//! Which Proton runs it depends first on whether the game is Steam's edition,
//! which ships `steam_api.dll` next to its executables. A Steam copy runs on
//! the Proton Steam runs it with, and the launcher downloads nothing; the user
//! can opt in to the launcher's own GE-Proton instead. Anything else runs on
//! that GE-Proton (see [`managed_proton`]), downloaded the first time. Either
//! way a Proton folder of the user's own can take over ([`ProtonRunner`]).
//! Each runs inside the Steam Linux Runtime it was built for, as Steam would
//! run it.
//!
//! The prefix follows the same split. A Steam copy in its library keeps
//! Steam's (`steamapps/compatdata/<appid>/pfx`), so saves and `Options.ini`
//! are shared with Steam's own launch; anything else gets the launcher's own. Proton is Wine underneath, which is where the DLL
//! overrides and the prefix's registry come from.
//!
//! Everything here is plain path and string handling, so it compiles and is
//! tested on every platform; only non-Windows builds actually launch through it.
#![cfg_attr(windows, allow(dead_code))]

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::config;
use crate::game::managed_proton::{self, Managed, Progress};
use crate::game::steam::{self, SteamGame};
use crate::model::{ProtonRunner, ProtonSettings};
use crate::util::fs as gfs;

const DLL_OVERRIDES: &str = "WINEDLLOVERRIDES";

/// Which Proton build runs the game.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// The one Steam runs this game with.
    Steam(PathBuf),
    /// The GE-Proton the launcher downloads and keeps.
    Managed,
    /// A folder the user picked.
    Custom(PathBuf),
}

/// Which edition the game folder holds, and for Steam's, where Steam has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installation {
    /// Steam's edition, in a Steam library on this machine.
    Steam(SteamGame),
    /// Steam's edition, outside every Steam library: copied or moved out of
    /// one, or in a library this machine's Steam does not list.
    SteamElsewhere,
    /// Any other edition.
    Other,
}

impl Installation {
    pub fn is_steam(&self) -> bool {
        !matches!(self, Self::Other)
    }
}

/// The edition in the game folder.
pub fn installation() -> Installation {
    let dir = config::game_dir();
    if !steam::is_steam_copy(dir) {
        return Installation::Other;
    }
    SteamGame::detect(dir).map_or(Installation::SteamElsewhere, Installation::Steam)
}

/// What the settings ask for. `steam_proton` is the Proton Steam runs the
/// game with, if Steam has one for it.
fn choose(settings: &ProtonSettings, installation: &Installation, steam_proton: Option<PathBuf>) -> Result<Source> {
    let folder = settings.proton.trim();
    // A folder set before there was a choice meant "use this one".
    let custom = settings.runner == ProtonRunner::Custom
        || (settings.runner == ProtonRunner::Auto && !folder.is_empty());

    if custom {
        anyhow::ensure!(!folder.is_empty(), "pick a Proton folder, or choose another Proton");
        return proton_dir(folder)
            .map(Source::Custom)
            .with_context(|| format!("`{folder}` is not a Proton folder"));
    }
    match settings.runner {
        ProtonRunner::Managed => Ok(Source::Managed),
        // Steam's edition runs on Steam's Proton; the launcher does not stand
        // in for it unasked, since that means a 750 MB download.
        _ => match installation {
            Installation::Steam(_) => steam_proton.map(Source::Steam).context(
                "Steam has no Proton for the game yet. In Steam, pick one under the game's Properties → \
                 Compatibility and start the game once from Steam, or turn on the launcher's GE-Proton",
            ),
            Installation::SteamElsewhere => anyhow::bail!(
                "this is Steam's edition of the game (it has steam_api.dll), but it is not in a Steam \
                 library on this machine, so there is no Steam Proton for it. Turn on the launcher's \
                 GE-Proton, or run the copy in your Steam library"
            ),
            Installation::Other => Ok(Source::Managed),
        },
    }
}

/// The game's place in a Steam library, when it is Steam's edition and in one.
fn steam_game() -> Option<SteamGame> {
    match installation() {
        Installation::Steam(steam) => Some(steam),
        _ => None,
    }
}

/// What the settings ask for, for the game in the game folder.
fn source(settings: &ProtonSettings) -> Result<Source> {
    let installation = installation();
    let steam_proton = match &installation {
        Installation::Steam(steam) => steam.proton(),
        _ => None,
    };
    choose(settings, &installation, steam_proton)
}

/// The Proton folder for `source`, if it is on disk.
fn installed_dir(source: &Source, managed: &Managed) -> Option<PathBuf> {
    match source {
        Source::Steam(dir) | Source::Custom(dir) => Some(dir.clone()),
        Source::Managed => managed.proton_dir(),
    }
}

/// The runtime `proton_dir` asks to run inside, if it is on disk: Steam's, or
/// the launcher's own.
fn runtime_for(proton_dir: &Path, steam: Option<&SteamGame>, managed: &Managed) -> Option<PathBuf> {
    let appid = managed_proton::required_runtime(proton_dir)?;
    steam.and_then(|s| s.runtime_entry_point(proton_dir)).or_else(|| managed.runtime_entry(&appid))
}

/// Download whatever the chosen Proton still needs: the managed GE-Proton, and
/// the runtime it runs in when neither Steam nor the launcher has it. Cheap,
/// with no network, when everything is in place. Returns whether anything was
/// downloaded.
pub fn ensure_ready(settings: &ProtonSettings, progress: Progress) -> Result<bool> {
    let source = source(settings)?;
    let managed = Managed::default();
    let steam = steam_game();

    let mut downloaded = false;
    let dir = match installed_dir(&source, &managed) {
        Some(dir) => dir,
        None => {
            downloaded = true;
            managed.install_proton(false, progress)?
        }
    };
    if managed_proton::required_runtime(&dir).is_some() && runtime_for(&dir, steam.as_ref(), &managed).is_none() {
        let appid = managed_proton::required_runtime(&dir).unwrap_or_default();
        managed.install_runtime(&appid, false, progress)?;
        downloaded = true;
    }
    Ok(downloaded)
}

/// Bring the managed GE-Proton, and the runtime it runs in, up to the latest
/// releases. Returns the version now installed.
pub fn update_managed(progress: Progress) -> Result<String> {
    let managed = Managed::default();
    let dir = managed.install_proton(true, progress)?;
    if let Some(appid) = managed_proton::required_runtime(&dir) {
        managed.install_runtime(&appid, true, progress)?;
    }
    managed.version().context("GE-Proton was not installed")
}

/// The managed GE-Proton's version, if it has been downloaded.
pub fn managed_version() -> Option<String> {
    Managed::default().version()
}

/// The Wine prefix the game runs in: Steam's for Steam's edition in its
/// library, else the launcher's own. Which Proton runs the game does not
/// change it.
pub fn prefix() -> PathBuf {
    match steam_game() {
        Some(steam) => steam.prefix(),
        None => Managed::default().compat_data().join("pfx"),
    }
}

/// A Proton build, ready to run the game.
pub struct Proton {
    /// Proton's `proton` script.
    pub script: PathBuf,
    source: Source,
    /// The Steam install, for the variables Steam would set. `None` for a copy
    /// of the game outside Steam.
    steam: Option<SteamGame>,
    /// The launcher's own Proton, runtime and prefix.
    managed: Managed,
    /// The Steam Linux Runtime `_v2-entry-point` to run Proton inside.
    runtime: Option<PathBuf>,
    env: Vec<(String, String)>,
}

impl Proton {
    /// The Proton the settings ask for, from what is on disk. [`ensure_ready`]
    /// downloads what is missing first.
    pub fn resolve(settings: &ProtonSettings) -> Result<Self> {
        let source = source(settings)?;
        let managed = Managed::default();
        let dir = installed_dir(&source, &managed)
            .context("GE-Proton has not been downloaded yet. It is, the first time the game is started")?;
        let steam = steam_game();
        let runtime = runtime_for(&dir, steam.as_ref(), &managed);
        Ok(Self { script: dir.join("proton"), source, steam, managed, runtime, env: parse_env(&settings.env) })
    }

    /// `STEAM_COMPAT_DATA_PATH`: the prefix is `pfx` inside it.
    fn compat_data(&self) -> PathBuf {
        match &self.steam {
            Some(steam) => steam.compat_data.clone(),
            None => self.managed.compat_data(),
        }
    }

    /// The Wine prefix the game runs in.
    pub fn prefix(&self) -> PathBuf {
        self.compat_data().join("pfx")
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

        cmd.env("STEAM_COMPAT_DATA_PATH", self.compat_data())
            .env("STEAM_COMPAT_INSTALL_PATH", install_dir);

        let mut tools: Vec<&Path> = self.script.parent().into_iter().collect();
        tools.extend(self.runtime.as_deref().and_then(Path::parent));
        cmd.env("STEAM_COMPAT_TOOL_PATHS", join_paths(&tools));

        match &self.steam {
            Some(steam) => {
                cmd.env("STEAM_COMPAT_APP_ID", &steam.app_id)
                    .env("SteamAppId", &steam.app_id)
                    .env("SteamGameId", &steam.app_id);
                if let Some(client) = steam.client_dir() {
                    cmd.env("STEAM_COMPAT_CLIENT_INSTALL_PATH", client);
                }
                // Shared into the runtime container; Proton also maps it as drive S:.
                cmd.env("STEAM_COMPAT_LIBRARY_PATHS", &steam.steamapps);
            }
            None => {
                // No Steam: no app id, and a stand-in client folder, which
                // Proton only looks in for Steam's own libraries.
                let client = self.managed.steam_client_stub();
                let _ = std::fs::create_dir_all(&client);
                cmd.env("STEAM_COMPAT_APP_ID", "0")
                    .env("SteamAppId", "0")
                    .env("SteamGameId", "0")
                    .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", client);
                // The runtime container sees only the home folder unless told
                // otherwise; the game may be on another drive.
                let mut mounts = vec![install_dir, self.managed.root()];
                mounts.extend(install_dir.parent());
                cmd.env("STEAM_COMPAT_MOUNTS", join_paths(&mounts));
            }
        }
        cmd
    }

    /// The Proton build's name, e.g. `Proton - Experimental` or `GE-Proton10-25`.
    pub fn name(&self) -> String {
        self.script
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.script.display().to_string())
    }
}

fn join_paths(paths: &[&Path]) -> OsString {
    let mut joined = OsString::new();
    for (i, path) in paths.iter().enumerate() {
        if i > 0 {
            joined.push(":");
        }
        joined.push(path);
    }
    joined
}

/// One line describing what a launch would use, for the options screen.
pub fn describe(settings: &ProtonSettings) -> Result<String> {
    let prefix = prefix();
    let proton = match Proton::resolve(settings) {
        Ok(proton) => proton,
        // Not an error: the first launch downloads it.
        Err(_) if source(settings)? == Source::Managed => {
            return Ok(format!(
                "GE-Proton is downloaded the first time the game starts (about 750 MB, into {}) · prefix {}",
                Managed::default().root().display(),
                prefix.display()
            ));
        }
        Err(e) => return Err(e),
    };
    let from = match (&proton.source, &proton.steam) {
        (Source::Steam(_), Some(steam)) => format!(" (Steam's, for app {})", steam.app_id),
        (Source::Managed, _) => " (downloaded by the launcher)".to_owned(),
        _ => String::new(),
    };
    let runtime = if proton.runtime.is_some() { ", in the Steam Linux Runtime" } else { "" };
    Ok(format!("{}{from}{runtime} · prefix {}", proton.name(), prefix.display()))
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

/// A Proton folder, given as the folder or its `proton` script.
fn proton_dir(configured: &str) -> Option<PathBuf> {
    let path = expand_home(configured.trim());
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

    fn settings(runner: ProtonRunner, folder: &Path) -> ProtonSettings {
        ProtonSettings { runner, proton: folder.display().to_string(), ..Default::default() }
    }

    fn in_steam_library() -> Installation {
        Installation::Steam(SteamGame {
            app_id: "2732960".into(),
            steamapps: PathBuf::from("/steam/steamapps"),
            compat_data: PathBuf::from("/steam/steamapps/compatdata/2732960"),
        })
    }

    #[test]
    fn steams_edition_runs_on_steams_proton_and_any_other_on_the_launchers() {
        let steams = PathBuf::from("/steam/Proton - Experimental");
        let auto = ProtonSettings::default();

        assert_eq!(choose(&auto, &in_steam_library(), Some(steams.clone())).unwrap(), Source::Steam(steams));
        assert_eq!(choose(&auto, &Installation::Other, None).unwrap(), Source::Managed);
    }

    #[test]
    fn steams_edition_without_a_steam_proton_is_explained_not_downloaded_for() {
        let never_started = choose(&ProtonSettings::default(), &in_steam_library(), None).unwrap_err();
        let message = format!("{never_started:#}");
        assert!(message.contains("start the game once from Steam"), "{message}");
        assert!(message.contains("turn on the launcher's GE-Proton"), "{message}");

        let moved = choose(&ProtonSettings::default(), &Installation::SteamElsewhere, None).unwrap_err();
        let message = format!("{moved:#}");
        assert!(message.contains("not in a Steam library"), "{message}");
        assert!(message.contains("Turn on the launcher's GE-Proton"), "{message}");
    }

    #[test]
    fn the_launchers_proton_can_be_turned_on_for_steams_edition() {
        let steams = PathBuf::from("/steam/Proton 9.0");
        let none = Path::new("");
        let managed = settings(ProtonRunner::Managed, none);

        assert_eq!(choose(&managed, &in_steam_library(), Some(steams)).unwrap(), Source::Managed);
        assert_eq!(choose(&managed, &in_steam_library(), None).unwrap(), Source::Managed);
        assert_eq!(choose(&managed, &Installation::SteamElsewhere, None).unwrap(), Source::Managed);
        assert_eq!(choose(&managed, &Installation::Other, None).unwrap(), Source::Managed);

        let no_folder = choose(&settings(ProtonRunner::Custom, none), &in_steam_library(), None).unwrap_err();
        assert!(format!("{no_folder:#}").contains("pick a Proton folder"), "{no_folder:#}");
    }

    #[test]
    fn a_config_that_named_steams_proton_reads_as_automatic() {
        let settings: ProtonSettings = serde_json::from_str(r#"{"Runner":"Steam"}"#).unwrap();
        assert_eq!(settings.runner, ProtonRunner::Auto);
    }

    #[test]
    fn a_proton_folder_is_used_as_chosen_or_as_an_older_config_set_it() {
        let dir = scratch("custom");
        fs::write(dir.join("proton"), b"").unwrap();

        for runner in [ProtonRunner::Custom, ProtonRunner::Auto] {
            assert_eq!(choose(&settings(runner, &dir), &in_steam_library(), None).unwrap(), Source::Custom(dir.clone()));
        }
        // Its `proton` script names it as well.
        assert_eq!(
            choose(&settings(ProtonRunner::Custom, &dir.join("proton")), &Installation::Other, None).unwrap(),
            Source::Custom(dir.clone())
        );
        let not_proton = choose(&settings(ProtonRunner::Custom, &dir.join("files")), &Installation::Other, None).unwrap_err();
        assert!(format!("{not_proton:#}").contains("is not a Proton folder"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// The variables a launch outside Steam gets, read off the command.
    #[test]
    fn outside_steam_proton_runs_in_the_launchers_prefix_with_no_app_id() {
        let root = scratch("outside-steam");
        let managed = Managed::new(root.join("data"), managed_proton::Sources::default());
        let proton = Proton {
            script: root.join("GE-Proton/proton"),
            source: Source::Managed,
            steam: None,
            managed: managed.clone(),
            runtime: Some(root.join("data").join("runtime").join("steamrt-1628350").join("_v2-entry-point")),
            env: Vec::new(),
        };
        let game = root.join("games/Zero Hour");

        let cmd = proton.base_command("waitforexitandrun", &game);
        let env: std::collections::HashMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_string_lossy().into_owned(), v?.to_string_lossy().into_owned())))
            .collect();
        let data = root.join("data");
        let list = |paths: &[&Path]| join_paths(paths).to_string_lossy().into_owned();

        assert_eq!(Path::new(cmd.get_program()), data.join("runtime/steamrt-1628350/_v2-entry-point"));
        assert_eq!(Path::new(&env["STEAM_COMPAT_DATA_PATH"]), data.join("prefix"));
        assert_eq!(Path::new(&env["STEAM_COMPAT_CLIENT_INSTALL_PATH"]), data.join("steam"));
        assert!(data.join("steam").is_dir(), "the stand-in client folder must exist");
        assert_eq!(env["SteamAppId"], "0");
        assert_eq!(env["STEAM_COMPAT_APP_ID"], "0");
        assert_eq!(env["STEAM_COMPAT_MOUNTS"], list(&[&game, &data, &root.join("games")]));
        assert_eq!(
            env["STEAM_COMPAT_TOOL_PATHS"],
            list(&[&root.join("GE-Proton"), &data.join("runtime").join("steamrt-1628350")])
        );
        assert!(!env.contains_key("STEAM_COMPAT_LIBRARY_PATHS"));
        assert_eq!(proton.prefix(), data.join("prefix").join("pfx"));
        let _ = fs::remove_dir_all(&root);
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
