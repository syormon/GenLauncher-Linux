//! Game engines that come as a package of their own, rather than with a mod.
//!
//! Some projects replace the game's engine outright: a rebuilt executable with
//! its own libraries beside it (a renderer, video codecs), plus at most a few
//! loose files, and no `.big` archives at all. *Zero Hour Reimagined* is one:
//! `x64/generalszh64.exe` with DXVK and FFmpeg DLLs next to it, started from the
//! game folder by a `Play-*.bat` script.
//!
//! That is a different thing from a mod that ships a modified engine (see
//! [`custom_engine`](super::custom_engine)), where the mod's data is the point
//! and the engine only makes it load. Here the engine *is* the package. So:
//!
//! - every file of the package is linked into the game folder, its executables
//!   and libraries included, which no mod is allowed;
//! - the engine starts the way the package's own launch scripts start it. The
//!   scripts are read, never run: only `set NAME=value` lines and the line that
//!   starts the engine are understood, and the launcher does the starting;
//! - nothing asks which engine to use: there is only this one.
//!
//! What was found is kept in [`MANIFEST`] in the version's folder, so it travels
//! with the files and goes when they are deleted.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::game::custom_engine;
use crate::util::fs as gfs;

/// The file in a version folder that marks it as an engine package. Never
/// linked into the game folder.
pub const MANIFEST: &str = "GenLauncherEngine.json";

/// How deep below the package's top an engine may sit: `x64/` is one level.
/// No deeper, or an archive holding a package per game (`Zero Hour/x64/`,
/// `Generals/x64/`) would look like one package with both inside.
const ENGINE_DEPTH: usize = 1;

/// Launch scripts, read for how to start the engine and then left out.
const SCRIPT_EXTENSIONS: &[&str] = &["bat", "cmd"];

/// A version folder holding a game engine package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EnginePackage {
    /// The engine, relative to the version folder, and so to the game folder
    /// once linked: `x64/generalszh64.exe`.
    pub executable: String,
    /// Ways to start it, from the package's launch scripts. Never empty.
    pub profiles: Vec<LaunchProfile>,
    /// Which of `profiles` a launch uses.
    #[serde(default)]
    pub selected: usize,
    /// The package brings game data too (`.big` archives or a `Data`
    /// folder), and so replaces the game's, as a mod does. Most bring only
    /// the engine and run on the game's own data.
    #[serde(default)]
    pub has_game_data: bool,
}

/// One way of starting the engine, as one launch script did it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LaunchProfile {
    /// For the menu: the script's name without its `Play-` prefix.
    pub name: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// Is `version_folder` an engine package? Cheap: it only looks for the manifest.
pub fn is_package(version_folder: &Path) -> bool {
    version_folder.join(MANIFEST).is_file()
}

impl EnginePackage {
    /// The profile a launch uses.
    pub fn profile(&self) -> &LaunchProfile {
        self.profiles.get(self.selected).or(self.profiles.first()).expect("a package has a profile")
    }

    /// The package in `version_folder`, if it is one.
    pub fn read(version_folder: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(version_folder.join(MANIFEST)).ok()?;
        let package: Self = serde_json::from_str(&text)
            .inspect_err(|e| log::warn!("unreadable {} in {}: {e}", MANIFEST, version_folder.display()))
            .ok()?;
        (!package.profiles.is_empty()).then_some(package)
    }

    pub fn write(&self, version_folder: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(version_folder.join(MANIFEST), text)
            .with_context(|| format!("cannot write {MANIFEST} in {}", version_folder.display()))
    }
}

/// Does `dir` hold an engine package: an engine, other than those in
/// `known_engines`, in a subfolder, or at the top with no game data beside it?
/// An engine at the top next to `.big` archives is a mod that brings its own
/// engine, which [`custom_engine`] deals with.
pub fn find_engine(dir: &Path, known_engines: &[PathBuf]) -> Option<String> {
    find_engines(dir, known_engines).into_iter().next()
}

/// Every engine [`find_engine`] could pick, nearest first, then by name.
fn find_engines(dir: &Path, known_engines: &[PathBuf]) -> Vec<String> {
    let has_data = has_game_data(dir);
    let mut candidates: Vec<(usize, String)> = Vec::new();
    collect_executables(dir, dir, 0, &mut candidates);
    candidates.retain(|(depth, _)| *depth > 0 || !has_data);
    candidates.sort();
    candidates
        .into_iter()
        .map(|(_, rel)| rel)
        .filter(|rel| custom_engine::is_new_engine(&dir.join(rel), known_engines))
        .collect()
}

/// `.big` archives (stored as `.gib`) or a `Data` folder at the top.
fn has_game_data(dir: &Path) -> bool {
    gfs::list_files(dir).iter().any(|f| matches!(gfs::extension_of(f).as_str(), "big" | "gib"))
        || gfs::list_dirs(dir).iter().any(|d| gfs::file_name_of(d).eq_ignore_ascii_case("data"))
}

fn collect_executables(top: &Path, dir: &Path, depth: usize, found: &mut Vec<(usize, String)>) {
    for file in gfs::list_files(dir) {
        if gfs::extension_of(&file) == "exe" {
            if let Ok(rel) = file.strip_prefix(top) {
                found.push((depth, rel.to_string_lossy().replace('\\', "/")));
            }
        }
    }
    if depth < ENGINE_DEPTH {
        for sub in gfs::list_dirs(dir) {
            collect_executables(top, &sub, depth + 1, found);
        }
    }
}

/// Turn the freshly installed `version_folder` into an engine package if it is
/// one: work out how to start the engine from its launch scripts, record that
/// in [`MANIFEST`], and drop the scripts, which the launcher stands in for.
/// Otherwise make sure no manifest is left from an earlier install.
pub fn mark(version_folder: &Path, known_engines: &[PathBuf]) -> Result<Option<EnginePackage>> {
    let manifest = version_folder.join(MANIFEST);
    let _ = std::fs::remove_file(&manifest);

    let engines = find_engines(version_folder, known_engines);
    if engines.is_empty() {
        return Ok(None);
    }

    let scripts: Vec<PathBuf> = gfs::list_files(version_folder)
        .into_iter()
        .filter(|f| SCRIPT_EXTENSIONS.contains(&gfs::extension_of(f).as_str()))
        .collect();
    let texts: Vec<(String, String)> = scripts
        .iter()
        .filter_map(|script| {
            let text = String::from_utf8_lossy(&std::fs::read(script).ok()?).into_owned();
            Some((profile_name(script), text))
        })
        .collect();
    let profiles_for = |executable: &str| -> Vec<LaunchProfile> {
        let name = gfs::file_name_of(Path::new(executable));
        texts.iter().filter_map(|(profile, text)| parse_script(profile, text, &name)).collect()
    };

    // The engine the scripts start, if they start one; with several (say,
    // one per game in one folder), the one most of them start.
    let (executable, mut profiles) = engines
        .iter()
        .map(|engine| (engine.clone(), profiles_for(engine)))
        .max_by_key(|(_, profiles)| profiles.len())
        .filter(|(_, profiles)| !profiles.is_empty())
        .unwrap_or_else(|| (engines[0].clone(), Vec::new()));

    profiles.sort_by_key(|p| p.name.to_ascii_lowercase());
    if profiles.is_empty() {
        profiles.push(LaunchProfile { name: "Default".to_owned(), ..Default::default() });
    }

    // The plainest start is the author's main way in; the others add to it
    // (a different renderer, no fog, a farther camera). Ties go to the first
    // by name.
    let selected = profiles
        .iter()
        .enumerate()
        .min_by_key(|(_, p)| p.args.len() + p.env.len())
        .map_or(0, |(index, _)| index);
    let package = EnginePackage { executable, profiles, selected, has_game_data: has_game_data(version_folder) };
    package.write(version_folder)?;

    for script in scripts {
        let _ = std::fs::remove_file(script);
    }
    log::info!(
        "{} is a game engine package: {}, started as {:?}",
        version_folder.display(),
        package.executable,
        package.profile()
    );
    Ok(Some(package))
}

/// `Play-Updated.bat` → `Updated`; a leading "Play" word is dropped.
fn profile_name(script: &Path) -> String {
    let stem = script.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let words: Vec<&str> = stem.split(['-', '_', ' ']).filter(|w| !w.is_empty()).collect();
    let words = match words.split_first() {
        Some((first, rest)) if first.eq_ignore_ascii_case("play") && !rest.is_empty() => rest,
        _ => &words[..],
    };
    words.join(" ")
}

/// How the script `text` starts `engine_name`, or `None` if it does not.
///
/// Only two kinds of line mean anything: `set NAME=value`, and the line that
/// runs the engine, whose arguments are kept. `%*` (the script's own
/// arguments) and anything else with a `%` variable are dropped.
fn parse_script(name: &str, text: &str, engine_name: &str) -> Option<LaunchProfile> {
    let mut env = Vec::new();
    for line in text.lines().map(str::trim) {
        let lower = line.to_ascii_lowercase();
        if lower.is_empty() || lower.starts_with("rem ") || lower == "rem" || lower.starts_with("::") {
            continue;
        }
        if let Some(assignment) = lower.strip_prefix("set ").map(|_| line[4..].trim()) {
            let assignment = assignment.trim_matches('"');
            if let Some((key, value)) = assignment.split_once('=') {
                // `set /a` and `set /p` compute or prompt; neither is a setting.
                if !key.starts_with('/') && !key.trim().is_empty() {
                    env.push((key.trim().to_owned(), value.to_owned()));
                }
            }
            continue;
        }

        let words = split_words(line);
        let starts = words.iter().position(|word| {
            let path = word.replace('\\', "/");
            Path::new(&path).file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(engine_name))
        });
        if let Some(at) = starts {
            let args = words[at + 1..]
                .iter()
                // `& pause`, `>nul`, `| more`: the command ends there.
                .take_while(|w| !ends_command(w))
                .filter(|w| !w.contains('%'))
                .cloned()
                .collect();
            return Some(LaunchProfile { name: name.to_owned(), args, env });
        }
    }
    None
}

/// Does `word` end the command before it: `&`, `&&`, `|`, `||`, or a
/// redirection such as `>nul`, `2>&1` or `<input`?
fn ends_command(word: &str) -> bool {
    let unnumbered = word.trim_start_matches(|c: char| c.is_ascii_digit());
    word.starts_with(['&', '|']) || unnumbered.starts_with(['>', '<'])
}

/// A command line split into words, `"quoted words"` kept whole.
fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Enough of an engine for `custom_engine` to take it for one.
    fn engine(seed: u8) -> Vec<u8> {
        let mut bytes = vec![seed; 1_200_000];
        bytes.extend_from_slice(b"CommandButton ObjectCreationList PlayerTemplate");
        bytes
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gl-engine-package-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const PLAY_UPDATED: &str = "@echo off\r\nrem 64-bit Zero Hour.\r\ncd /d \"%~dp0\"\r\n\"%~dp0x64\\generalszh64.exe\" -useCwd %*\r\n";
    const PLAY_ORIGINAL: &str = "@echo off\r\ncd /d \"%~dp0\"\r\nset GENERALS_GFX=d3d8\r\nset GENERALS_ORIGINAL=1\r\n\"%~dp0x64\\generalszh64.exe\" -useCwd %*\r\n";

    #[test]
    fn reads_how_a_script_starts_the_engine() {
        let updated = parse_script("Updated", PLAY_UPDATED, "generalszh64.exe").unwrap();
        assert_eq!(updated.args, ["-useCwd"]);
        assert!(updated.env.is_empty());

        let original = parse_script("Original", PLAY_ORIGINAL, "GeneralsZH64.exe").unwrap();
        assert_eq!(original.args, ["-useCwd"]);
        assert_eq!(
            original.env,
            [("GENERALS_GFX".to_owned(), "d3d8".to_owned()), ("GENERALS_ORIGINAL".to_owned(), "1".to_owned())]
        );

        // A script that does something else entirely says nothing.
        let other = "@echo off\r\npowershell -NoProfile -Command \"Move-Item a b\"\r\npause\r\n";
        assert_eq!(parse_script("Skip", other, "generalszh64.exe"), None);
        // A comment that mentions the engine is not a start.
        assert_eq!(parse_script("X", "rem run x64\\generalszh64.exe -win\r\n", "generalszh64.exe"), None);

        // What follows the command is not its arguments.
        for line in [
            "x64\\engine.exe -win & pause",
            "x64\\engine.exe -win && exit",
            "x64\\engine.exe -win >nul 2>&1",
            "x64\\engine.exe -win 2>log.txt",
            "x64\\engine.exe -win | more",
        ] {
            let started = parse_script("X", line, "engine.exe").unwrap();
            assert_eq!(started.args, ["-win"], "{line}");
        }
        // `start "title" engine args` starts it too.
        let started = parse_script("X", "start \"\" \"%~dp0x64\\engine.exe\" -useCwd", "engine.exe").unwrap();
        assert_eq!(started.args, ["-useCwd"]);
    }

    #[test]
    fn the_scripts_say_which_engine_and_the_plainest_start_is_the_default() {
        let dir = scratch("choice");
        fs::create_dir_all(dir.join("bin")).unwrap();
        // Two engines; by name alone the wrong one would come first.
        fs::write(dir.join("bin/a_generals.exe"), engine(6)).unwrap();
        fs::write(dir.join("bin/b_zerohour.exe"), engine(7)).unwrap();
        for (script, line) in [
            ("Play-FarZoom.bat", "bin\\b_zerohour.exe -useCwd -maxZoom 750"),
            ("Play-NoFog.bat", "bin\\b_zerohour.exe -useCwd -noFog"),
            ("Play-Original.bat", "set GFX=d3d8\r\nbin\\b_zerohour.exe -useCwd"),
            ("Play-Updated.bat", "bin\\b_zerohour.exe -useCwd"),
        ] {
            fs::write(dir.join(script), line).unwrap();
        }

        let package = mark(&dir, &[]).unwrap().expect("a package");
        assert_eq!(package.executable, "bin/b_zerohour.exe");
        assert_eq!(package.profiles.len(), 4);
        assert_eq!(package.profile().name, "Updated");
        assert!(!package.has_game_data);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_a_profile_after_its_script() {
        assert_eq!(profile_name(Path::new("Play-Updated.bat")), "Updated");
        assert_eq!(profile_name(Path::new("Play-NoFog-FarZoom.bat")), "NoFog FarZoom");
        assert_eq!(profile_name(Path::new("Start.cmd")), "Start");
        assert_eq!(profile_name(Path::new("Play.bat")), "Play");
        assert_eq!(profile_name(Path::new("Player_Two.bat")), "Player Two");
    }

    #[test]
    fn an_engine_in_a_subfolder_is_a_package_and_its_scripts_say_how_to_start_it() {
        let dir = scratch("reimagined");
        fs::create_dir_all(dir.join("x64")).unwrap();
        fs::create_dir_all(dir.join("Art/Textures")).unwrap();
        fs::write(dir.join("x64/generalszh64.exe"), engine(1)).unwrap();
        fs::write(dir.join("x64/d3d8.dll"), b"dxvk").unwrap();
        fs::write(dir.join("Art/Textures/ModIntroLogo.tga"), b"tga").unwrap();
        fs::write(dir.join("Play-Updated.bat"), PLAY_UPDATED).unwrap();
        fs::write(dir.join("Play-Original.bat"), PLAY_ORIGINAL).unwrap();
        fs::write(dir.join("Skip-IntroMovies.bat"), "@echo off\r\npowershell -Command \"x\"\r\n").unwrap();

        let package = mark(&dir, &[]).unwrap().expect("a package");
        assert_eq!(package.executable, "x64/generalszh64.exe");
        let names: Vec<&str> = package.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Original", "Updated"]);
        // The plain start is the default.
        assert_eq!(package.profile().name, "Updated");

        // Recorded with the files; the scripts are gone, the rest stays.
        assert_eq!(EnginePackage::read(&dir), Some(package));
        assert!(!dir.join("Play-Updated.bat").exists() && !dir.join("Skip-IntroMovies.bat").exists());
        assert!(dir.join("x64/d3d8.dll").exists() && dir.join("Art/Textures/ModIntroLogo.tga").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mod_with_its_own_engine_or_a_copy_of_ours_is_not_a_package() {
        // Engine at the top beside the mod's archives: custom_engine's case.
        let dir = scratch("mod");
        fs::write(dir.join("generals.exe"), engine(2)).unwrap();
        fs::write(dir.join("Raptor.gib"), b"BIGF").unwrap();
        assert_eq!(mark(&dir, &[]).unwrap(), None);
        assert!(!dir.join(MANIFEST).exists());

        // Our own engine in a subfolder changes nothing.
        let known = scratch("known").join("game.dat");
        fs::write(&known, engine(3)).unwrap();
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin/game.exe"), engine(3)).unwrap();
        assert_eq!(find_engine(&dir, std::slice::from_ref(&known)), None);
        // A different one there is a package, even beside archives, and
        // one that brings game data of its own.
        fs::write(dir.join("bin/game.exe"), engine(4)).unwrap();
        assert_eq!(find_engine(&dir, std::slice::from_ref(&known)).as_deref(), Some("bin/game.exe"));
        let package = mark(&dir, &[known]).unwrap().expect("a package");
        assert!(package.has_game_data);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_package_without_scripts_still_starts() {
        let dir = scratch("bare");
        fs::write(dir.join("engine.exe"), engine(5)).unwrap();
        let package = mark(&dir, &[]).unwrap().expect("a package");
        assert_eq!(package.executable, "engine.exe");
        assert_eq!(package.profile(), &LaunchProfile { name: "Default".into(), ..Default::default() });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reinstall_as_something_else_drops_the_old_manifest() {
        let dir = scratch("reinstall");
        fs::write(dir.join(MANIFEST), "{}").unwrap();
        fs::write(dir.join("Mod.gib"), b"BIGF").unwrap();
        assert_eq!(mark(&dir, &[]).unwrap(), None);
        assert!(!dir.join(MANIFEST).exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
