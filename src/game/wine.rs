//! Running the Windows game under Wine, until a native Linux build exists.
//!
//! Everything here is plain path and string handling, so it compiles and is
//! tested on every platform; only non-Windows builds actually launch through it.
#![cfg_attr(windows, allow(dead_code))]

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::config;
use crate::model::WineSettings;
use crate::util::fs as gfs;

const DLL_OVERRIDES: &str = "WINEDLLOVERRIDES";

/// A Wine installation paired with the prefix the game lives in.
pub struct Wine {
    pub program: PathBuf,
    pub prefix: PathBuf,
    env: Vec<(String, String)>,
}

impl Wine {
    pub fn resolve(settings: &WineSettings) -> Result<Self> {
        let program = find_program(&settings.binary).with_context(|| {
            if settings.binary.trim().is_empty() {
                "no `wine` was found on PATH. Install Wine, or set its location under Options → Wine".to_owned()
            } else {
                format!("the Wine binary `{}` does not exist", settings.binary.trim())
            }
        })?;

        Ok(Self { program, prefix: effective_prefix(settings), env: parse_env(&settings.env) })
    }

    /// A command that runs the Windows program `exe` from `cwd`.
    pub fn command(&self, exe: &Path, cwd: &Path) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.arg(exe).current_dir(cwd);
        cmd.env("WINEPREFIX", &self.prefix);

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

    /// `wine --version`, e.g. `wine-9.0`.
    pub fn version(&self) -> Option<String> {
        let output = Command::new(&self.program).arg("--version").output().ok()?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (output.status.success() && !text.is_empty()).then_some(text)
    }
}

/// One line describing what a launch would use, for the options screen.
pub fn describe(settings: &WineSettings) -> Result<String> {
    let wine = Wine::resolve(settings)?;
    let version = wine.version().unwrap_or_else(|| wine.program.display().to_string());
    Ok(format!("{version} · prefix {}", wine.prefix.display()))
}

/// Open `winecfg` for the game's prefix without waiting for it.
pub fn open_winecfg(settings: &WineSettings) -> Result<()> {
    let wine = Wine::resolve(settings)?;
    let mut cmd = Command::new(&wine.program);
    cmd.arg("winecfg").env("WINEPREFIX", &wine.prefix);
    for (key, value) in &wine.env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().context("cannot start winecfg")?;
    // Reap it when it closes so it does not linger as a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

// ---------------------------------------------------------------------------
// Locating Wine and the prefix
// ---------------------------------------------------------------------------

/// The configured binary, else `wine` (or `wine64`) from PATH. A configured
/// folder is treated as a runner such as Lutris or Wine-GE ships: `bin/wine`.
fn find_program(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if configured.is_empty() {
        return which("wine").or_else(|| which("wine64"));
    }

    let path = expand_home(configured);
    if path.is_dir() {
        return ["bin/wine", "wine", "bin/wine64", "wine64"]
            .iter()
            .map(|rel| path.join(rel))
            .find(|p| p.is_file());
    }
    if path.is_file() {
        return Some(path);
    }
    // A bare name such as `wine-staging` is looked up on PATH.
    if !configured.contains('/') {
        return which(configured);
    }
    None
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| p.is_file())
}

/// The prefix a launch uses, most specific first: the setting, a `WINEPREFIX`
/// in the setting's environment, the prefix the game folder sits inside, the
/// inherited `WINEPREFIX`, and finally Wine's own default of `~/.wine`.
pub fn effective_prefix(settings: &WineSettings) -> PathBuf {
    let configured = settings.prefix.trim();
    if !configured.is_empty() {
        return expand_home(configured);
    }
    if let Some((_, value)) = parse_env(&settings.env).into_iter().find(|(k, _)| k == "WINEPREFIX") {
        return expand_home(&value);
    }
    if let Some(prefix) = detect_prefix(config::game_dir()) {
        return prefix;
    }
    if let Some(prefix) = std::env::var_os("WINEPREFIX").filter(|p| !p.is_empty()) {
        return PathBuf::from(prefix);
    }
    dirs::home_dir().unwrap_or_default().join(".wine")
}

/// The prefix `dir` is installed in: the parent of an enclosing `drive_c` that
/// holds Wine's registry files.
pub fn detect_prefix(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .filter(|a| a.file_name().is_some_and(|n| n == "drive_c"))
        .filter_map(Path::parent)
        .find(|prefix| prefix.join("system.reg").is_file() || prefix.join("user.reg").is_file())
        .map(Path::to_path_buf)
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
    let mut names: Vec<String> = std::env::var("USER").into_iter().collect();
    // Proton prefixes always use `steamuser`; otherwise any non-shared profile.
    names.push("steamuser".to_owned());
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
    fn finds_the_prefix_a_game_is_installed_in() {
        let prefix = scratch("prefix");
        fs::write(prefix.join("system.reg"), b"").unwrap();
        let game = prefix.join("drive_c").join("Games").join("Zero Hour");
        fs::create_dir_all(&game).unwrap();

        assert_eq!(detect_prefix(&game), Some(prefix.clone()));
        // A drive_c without registry files is not a prefix.
        fs::remove_file(prefix.join("system.reg")).unwrap();
        assert_eq!(detect_prefix(&game), None);

        let _ = fs::remove_dir_all(&prefix);
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
    fn matches_wine_processes_by_their_windows_name() {
        let wine_style = b"C:\\Games\\Zero Hour\\game.dat\0-win\0";
        let preloader = b"/usr/bin/wine64-preloader\0/usr/bin/wine64\0Z:\\zh\\GAME.DAT\0";
        let unrelated = b"/usr/bin/bash\0-c\0echo game.dat is here\0";

        assert!(cmdline_mentions(wine_style, &["game.dat"]));
        assert!(cmdline_mentions(preloader, &["game.dat"]));
        assert!(!cmdline_mentions(unrelated, &["game.dat"]));
    }
}
