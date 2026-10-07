//! Installing a mod from an archive the user downloaded themselves.
//!
//! Archives from sites such as ModDB follow no convention. Most are flat, with
//! the `.big` files at the top. Some wrap everything in a folder, and some are
//! a complete copy of the game with the mod applied. This module turns any of
//! those into the layout the launcher links from: a folder whose contents
//! mirror the game folder, holding only what the mod adds or changes.

use anyhow::{bail, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::config::{self, Game};
use crate::game::{launcher, stock_files};
use crate::util::archive;
use crate::util::fs as gfs;

/// How far down to look for the folder that mirrors the game folder.
const MAX_WRAPPER_DEPTH: usize = 6;

/// What an install did, for the summary shown afterwards.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// Files left out because the game already has an identical copy.
    pub skipped_files: usize,
    pub skipped_bytes: u64,
}

impl InstallReport {
    pub fn merge(&mut self, other: &InstallReport) {
        self.skipped_files += other.skipped_files;
        self.skipped_bytes += other.skipped_bytes;
    }
}

/// The steps of an install, in order, for the progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Unpacking the archive: nearly all of the time for a large mod.
    Unpacking,
    /// Checking which unpacked files the game already has.
    Comparing,
    /// Moving the result into the mod store.
    Finishing,
}

impl Stage {
    pub const COUNT: usize = 3;

    pub fn number(self) -> usize {
        self as usize + 1
    }

    pub fn label(self) -> String {
        crate::i18n::tr(match self {
            Stage::Unpacking => "Unpacking",
            Stage::Comparing => "ArchiveComparing",
            Stage::Finishing => "ArchiveFinishing",
        })
    }
}

/// Unpack `archive` and install what it holds into `target`, a version folder
/// in the mod store. `progress` is told which stage is running and how far
/// through it is, or `None` when that cannot be known.
pub fn install(
    archive_path: &Path,
    target: &Path,
    game: Game,
    progress: &dyn Fn(Stage, Option<f32>),
) -> Result<InstallReport> {
    // Staged beside the target under the suffix that start-up cleans away,
    // so an interrupted install never leaves a half-written mod behind.
    let staging = PathBuf::from(format!(
        "{}{}",
        target.display(),
        config::VERSION_FOLDER_COPY_SUFFIX
    ));
    let _ = std::fs::remove_dir_all(&staging);

    let result = (|| {
        archive::extract_with_progress(archive_path, &staging, true, &|fraction| {
            progress(Stage::Unpacking, fraction);
        })?;

        let root = find_game_root(&staging, game);
        if !contains_game_root(&root, 0) {
            bail!(
                "{} has no .big files or Data folder, so there is nothing to install. \
                 If it holds an installer, run that yourself.",
                gfs::file_name_of(archive_path)
            );
        }

        progress(Stage::Comparing, None);
        let report = drop_files_the_game_already_has(&root, game, &|fraction| {
            progress(Stage::Comparing, Some(fraction));
        });

        progress(Stage::Finishing, None);
        merge_into(&root, target)?;
        Ok(report)
    })();

    let _ = std::fs::remove_dir_all(&staging);
    result
}

// ---------------------------------------------------------------------------
// Name and version from the file name
// ---------------------------------------------------------------------------

/// A starting point for the name and version fields, from an archive's file
/// name: `Project_Raptor_War_Commanders_9.1.27_ENG_Voice.rar` gives
/// `Project Raptor War Commanders` and `9.1.27`. Either may come back wrong or
/// empty; the user confirms both.
pub fn guess_name_and_version(file_name: &str) -> (String, String) {
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or(file_name);
    let tokens: Vec<&str> =
        stem.split(['_', '-', ' ']).filter(|t| !t.is_empty()).collect();

    // The version is the last run of version-like tokens, so a number inside
    // the name ("Generals 2 Mod v1.5") is not mistaken for it. The first token
    // is always part of the name.
    let mut run: Option<(usize, usize)> = None;
    let mut i = 1;
    while i < tokens.len() {
        if !starts_version(tokens[i]) {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        // `1_5_5` is 1.5.5, and `Beta_V1` belongs together.
        while i < tokens.len()
            && (is_numeric(tokens[i])
                || (!has_digit(tokens[i - 1]) && starts_version(tokens[i])))
        {
            i += 1;
        }
        run = Some((start, i));
    }

    let Some((start, end)) = run else {
        return (tokens.join(" "), String::new());
    };

    let mut version = String::new();
    for (n, token) in tokens[start..end].iter().enumerate() {
        let token = strip_v(token);
        if n > 0 {
            version.push(if is_numeric(token) && has_digit(&version) { '.' } else { ' ' });
        }
        version.push_str(token);
    }

    (tokens[..start].join(" "), version)
}

fn has_digit(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
}

/// `155`, `9.1.27`.
fn is_numeric(token: &str) -> bool {
    token.starts_with(|c: char| c.is_ascii_digit())
        && token.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// `V1` → `1`; anything else is returned unchanged.
fn strip_v(token: &str) -> &str {
    match token.strip_prefix(['v', 'V']) {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
        _ => token,
    }
}

/// Could `token` begin a version? `9.1.27`, `v1`, `1.55a`, `Beta`, `BetaV1`.
fn starts_version(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let body = strip_v(&lower);

    // Digits and dots, optionally one trailing letter (`1.55a`).
    let numeric = body.starts_with(|c: char| c.is_ascii_digit()) && {
        let digits = body.trim_end_matches(|c: char| c.is_ascii_lowercase());
        body.len() - digits.len() <= 1 && is_numeric(digits)
    };

    numeric
        || ["beta", "alpha", "rc", "build", "release"].iter().any(|word| {
            lower.strip_prefix(word).is_some_and(|rest| rest.is_empty() || has_digit(rest))
        })
}

// ---------------------------------------------------------------------------
// Finding the folder that mirrors the game folder
// ---------------------------------------------------------------------------

/// The folder under `dir` whose contents belong in the game folder: the first
/// level that holds `.big` files or a `Data` folder. Wrapper folders above it
/// are stepped through, and when an archive carries a copy of both Generals
/// and Zero Hour, the one for `game` is chosen.
pub fn find_game_root(dir: &Path, game: Game) -> PathBuf {
    let mut current = dir.to_path_buf();

    for _ in 0..MAX_WRAPPER_DEPTH {
        if is_game_root(&current) {
            break;
        }

        let candidates: Vec<PathBuf> =
            subdirs(&current).into_iter().filter(|d| contains_game_root(d, 1)).collect();

        current = match candidates.len() {
            0 => break,
            1 => candidates.into_iter().next().expect("one candidate"),
            _ => candidates
                .into_iter()
                // max_by_key keeps the last maximum; reverse so the first wins.
                .rev()
                .max_by_key(|c| game_score(c, game))
                .expect("several candidates"),
        };
    }

    current
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect()
}

/// The stock name a mod file stands for: archives are stored as `.gib`.
fn stock_name(path: &Path) -> String {
    let name = gfs::file_name_of(path).to_ascii_lowercase();
    match name.strip_suffix(".gib") {
        Some(stem) => format!("{stem}.big"),
        None => name,
    }
}

fn is_archive_name(path: &Path) -> bool {
    matches!(gfs::extension_of(path).as_str(), "big" | "gib")
}

fn is_game_root(dir: &Path) -> bool {
    files_in(dir).iter().any(|f| is_archive_name(f))
        || subdirs(dir).iter().any(|d| gfs::file_name_of(d).eq_ignore_ascii_case("data"))
}

fn contains_game_root(dir: &Path, depth: usize) -> bool {
    is_game_root(dir)
        || (depth < MAX_WRAPPER_DEPTH
            && subdirs(dir).iter().any(|d| contains_game_root(d, depth + 1)))
}

/// How well the game copy under `candidate` matches `game`. Stock archives
/// that only one of the two games ships decide it; the folder name breaks ties.
fn game_score(candidate: &Path, game: Game) -> i64 {
    let zero_hour = stock_files::for_game(Game::ZeroHour);
    let generals = stock_files::for_game(Game::Generals);

    let root = find_game_root(candidate, game);
    let mut zh_only = 0i64;
    let mut generals_only = 0i64;
    for file in files_in(&root).iter().filter(|f| is_archive_name(f)) {
        let name = stock_name(file);
        match (zero_hour.contains(&name), generals.contains(&name)) {
            (true, false) => zh_only += 1,
            (false, true) => generals_only += 1,
            _ => {}
        }
    }

    let folder = gfs::file_name_of(candidate).to_ascii_lowercase();
    let named_zero_hour = folder.contains("zero hour") || folder.contains("zerohour");

    let (matching, other, name_hint) = match game {
        Game::ZeroHour => (zh_only, generals_only, named_zero_hour),
        Game::Generals => (generals_only, zh_only, !named_zero_hour),
    };
    (matching - other) * 2 + i64::from(name_hint)
}

// ---------------------------------------------------------------------------
// Leaving out what the game already has
// ---------------------------------------------------------------------------

/// Delete every file under `mod_root` that is byte-identical to the file the
/// game has in the same place. A full-game repack shrinks to just the mod.
///
/// Only files the game will still see at launch are candidates: before a
/// launch the launcher hides every non-stock `.big` and every loose file with
/// a mod extension, so an identical copy of one of *those* would vanish along
/// with the game's and must stay in the mod.
fn drop_files_the_game_already_has(
    mod_root: &Path,
    game: Game,
    progress: &dyn Fn(f32),
) -> InstallReport {
    let stock = stock_files::for_game(game);
    let mut report = InstallReport::default();

    // First the cheap checks, to find the files worth reading at all. Then
    // the reading, which is where the time goes and what progress measures.
    let mut candidates: Vec<(PathBuf, PathBuf, u64)> = Vec::new();
    gfs::visit_files(mod_root, &mut |file| {
        let Ok(rel) = file.strip_prefix(mod_root) else { return };
        let name = stock_name(file);

        let survives_launch = if is_archive_name(file) {
            stock.contains(&name)
        } else {
            !launcher::CUSTOM_FILE_EXTENSIONS.contains(&gfs::extension_of(file).as_str())
        };
        if !survives_launch {
            return;
        }

        let in_game = config::game_path(rel.with_file_name(&name));
        // Only a real file counts: a link would be another mod's, not the game's.
        let Ok(game_meta) = std::fs::symlink_metadata(&in_game) else { return };
        let Ok(mod_meta) = std::fs::metadata(file) else { return };
        if game_meta.file_type().is_file() && game_meta.len() == mod_meta.len() {
            candidates.push((file.to_path_buf(), in_game, mod_meta.len()));
        }
    });

    let total: u64 = candidates.iter().map(|(_, _, size)| size).sum();
    let mut done = 0u64;
    for (file, in_game, size) in candidates {
        let identical = same_contents(&file, &in_game, &mut |read| {
            if total > 0 {
                progress(((done + read) as f64 / total as f64).min(1.0) as f32);
            }
        });
        done += size;

        if identical && std::fs::remove_file(&file).is_ok() {
            report.skipped_files += 1;
            report.skipped_bytes += size;
        }
    }
    progress(1.0);

    remove_empty_dirs(mod_root);
    report
}

/// True when the two files hold the same bytes. `on_read` is told how many
/// bytes of them have been compared so far.
fn same_contents(a: &Path, b: &Path, on_read: &mut dyn FnMut(u64)) -> bool {
    let (Ok(meta_a), Ok(meta_b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if meta_a.len() != meta_b.len() {
        return false;
    }

    let (Ok(mut file_a), Ok(mut file_b)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let mut buf_a = vec![0u8; 1 << 20];
    let mut buf_b = vec![0u8; 1 << 20];
    let mut compared = 0u64;
    loop {
        let Ok(read) = file_a.read(&mut buf_a) else { return false };
        if read == 0 {
            return true;
        }
        if file_b.read_exact(&mut buf_b[..read]).is_err() || buf_a[..read] != buf_b[..read] {
            return false;
        }
        compared += read as u64;
        on_read(compared);
    }
}

/// Remove folders left empty, deepest first. `dir` itself is kept.
fn remove_empty_dirs(dir: &Path) {
    for sub in subdirs(dir) {
        remove_empty_dirs(&sub);
        // Fails, harmlessly, when the folder still has something in it.
        let _ = std::fs::remove_dir(&sub);
    }
}

/// Move everything under `from` into `to`, replacing files already there.
fn merge_into(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            if target.exists() {
                merge_into(&entry.path(), &target)?;
            } else {
                gfs::move_dir(&entry.path(), &target)?;
            }
        } else {
            let _ = std::fs::remove_file(&target);
            gfs::move_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gl-modarchive-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: PathBuf) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    /// Installs a real archive into a scratch folder and prints what came out.
    /// The game folder is only read, to find files the archive duplicates.
    ///
    /// ```text
    /// GENLAUNCHER_TEST_GAME_DIR=<game folder> GENLAUNCHER_TEST_ARCHIVE=<archive> \
    /// GENLAUNCHER_TEST_OUT=<empty scratch folder> \
    ///   cargo test live_install_of_a_real_archive -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real game folder and archive; see the doc comment"]
    fn live_install_of_a_real_archive() {
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let (Some(game_dir), Some(archive_path), Some(out)) = (
            var("GENLAUNCHER_TEST_GAME_DIR"),
            var("GENLAUNCHER_TEST_ARCHIVE"),
            var("GENLAUNCHER_TEST_OUT"),
        ) else {
            eprintln!("skipped: the GENLAUNCHER_TEST_* variables are not set");
            return;
        };

        config::set_game_dir(game_dir.clone());
        assert_eq!(config::game_dir(), game_dir, "another test claimed the game folder first");
        let game = crate::tasks::detect_game().expect("not a Generals or Zero Hour folder");

        let (name, version) = guess_name_and_version(&gfs::file_name_of(&archive_path));
        println!("guessed name: {name:?}, version: {version:?}");

        let started = std::time::Instant::now();
        // One line per stage and per ten percent, so the progress is visible.
        let last = std::cell::Cell::new((0usize, -1i32));
        let report = install(&archive_path, &out, game, &|stage, fraction| {
            let tenth = fraction.map_or(-1, |f| (f * 10.0) as i32);
            if last.get() != (stage.number(), tenth) {
                last.set((stage.number(), tenth));
                let shown = fraction.map_or("...".to_owned(), |f| format!("{:.0}%", f * 100.0));
                println!("progress at {:>4.0?}: {}/{} {} {shown}", started.elapsed(), stage.number(), Stage::COUNT, stage.label());
            }
        })
        .expect("install failed");

        let mut files = 0usize;
        let mut bytes = 0u64;
        gfs::visit_files(&out, &mut |f| {
            files += 1;
            bytes += fs::metadata(f).map(|m| m.len()).unwrap_or(0);
        });

        println!(
            "installed {files} files, {:.2} GB, in {:.0?}; left out {} identical files, {:.2} GB",
            bytes as f64 / 1e9,
            started.elapsed(),
            report.skipped_files,
            report.skipped_bytes as f64 / 1e9,
        );
        let mut top: Vec<String> = fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() { format!("{name}/") } else { name }
            })
            .collect();
        top.sort();
        println!("top level: {}", top.join(", "));

        assert!(is_game_root(&out), "the installed folder does not mirror a game folder");
        let staging =
            PathBuf::from(format!("{}{}", out.display(), config::VERSION_FOLDER_COPY_SUFFIX));
        assert!(!staging.exists(), "the staging folder was left behind");
    }

    #[test]
    fn guesses_name_and_version_from_real_file_names() {
        let guess = |name: &str| {
            let (name, version) = guess_name_and_version(name);
            format!("{name} | {version}")
        };

        assert_eq!(
            guess("Project_Raptor_War_Commanders_9.1.27_ENG_Voice.rar"),
            "Project Raptor War Commanders | 9.1.27"
        );
        assert_eq!(guess("CCTDRDX_1_5_5.zip"), "CCTDRDX | 1.5.5");
        assert_eq!(guess("CCG2_155_CN.zip"), "CCG2 | 155");
        assert_eq!(guess("DeepImpact_BetaV1.zip"), "DeepImpact | BetaV1");
        assert_eq!(guess("0GX_USBeta_V1.rar"), "0GX USBeta | 1");
        // A number inside the name is not the version; the last run is.
        assert_eq!(guess("Generals_2_Mod_v1.5.zip"), "Generals 2 Mod | 1.5");
        assert_eq!(guess("Some-Mod-Beta-V2.7z"), "Some Mod | Beta 2");
        // Nothing version-like: leave it for the user.
        assert_eq!(guess("ShockWave.zip"), "ShockWave | ");
    }

    #[test]
    fn a_flat_archive_is_its_own_root() {
        let dir = scratch("flat");
        touch(dir.join("!Mod.gib"));
        touch(dir.join("ReadMe.txt"));
        touch(dir.join("reshade-shaders/Shaders/a.fx"));

        assert_eq!(find_game_root(&dir, Game::ZeroHour), dir);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrapper_folders_are_stepped_through() {
        let dir = scratch("wrapped");
        touch(dir.join("ReadMe.txt"));
        touch(dir.join("My Mod v1/files/Data/INI/Object.ini"));

        assert_eq!(find_game_root(&dir, Game::ZeroHour), dir.join("My Mod v1/files"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_two_game_repack_yields_the_copy_for_the_managed_game() {
        let dir = scratch("twogames");
        let generals = dir.join("Command and Conquer Generals");
        let zero_hour = dir.join("Command and Conquer Generals Zero Hour");
        for name in ["INI.gib", "Window.gib", "Music.gib"] {
            touch(generals.join(name));
        }
        for name in ["INIZH.gib", "WindowZH.gib", "Music.gib"] {
            touch(zero_hour.join(name));
        }

        assert_eq!(find_game_root(&dir, Game::ZeroHour), zero_hour);
        assert_eq!(find_game_root(&dir, Game::Generals), generals);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_folder_name_decides_when_the_contents_cannot() {
        let dir = scratch("names");
        touch(dir.join("Generals/!Mod.gib"));
        touch(dir.join("Zero Hour/!Mod.gib"));

        assert_eq!(find_game_root(&dir, Game::ZeroHour), dir.join("Zero Hour"));
        assert_eq!(find_game_root(&dir, Game::Generals), dir.join("Generals"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_with_nothing_installable_is_recognised() {
        let dir = scratch("installer");
        touch(dir.join("Setup.exe"));
        touch(dir.join("docs/ReadMe.txt"));

        let root = find_game_root(&dir, Game::ZeroHour);
        assert!(!contains_game_root(&root, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compares_file_contents_not_just_sizes() {
        let dir = scratch("contents");
        fs::write(dir.join("a"), b"same bytes").unwrap();
        fs::write(dir.join("b"), b"same bytes").unwrap();
        fs::write(dir.join("c"), b"same_bytes").unwrap();
        fs::write(dir.join("d"), b"longer than the others").unwrap();

        let same = |a: &str, b: &str| same_contents(&dir.join(a), &dir.join(b), &mut |_| {});
        assert!(same("a", "b"));
        assert!(!same("a", "c"));
        assert!(!same("a", "d"));
        assert!(!same("a", "missing"));

        // The caller hears how much has been compared.
        let mut heard = 0;
        assert!(same_contents(&dir.join("a"), &dir.join("b"), &mut |read| heard = read));
        assert_eq!(heard, 10);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merging_keeps_existing_files_and_replaces_clashes() {
        let dir = scratch("merge");
        fs::create_dir_all(dir.join("from/Data")).unwrap();
        fs::create_dir_all(dir.join("to/Data")).unwrap();
        fs::write(dir.join("from/Data/new.ini"), b"new").unwrap();
        fs::write(dir.join("from/both.gib"), b"incoming").unwrap();
        fs::write(dir.join("to/Data/old.ini"), b"old").unwrap();
        fs::write(dir.join("to/both.gib"), b"existing").unwrap();

        merge_into(&dir.join("from"), &dir.join("to")).unwrap();

        assert_eq!(fs::read(dir.join("to/Data/new.ini")).unwrap(), b"new");
        assert_eq!(fs::read(dir.join("to/Data/old.ini")).unwrap(), b"old");
        assert_eq!(fs::read(dir.join("to/both.gib")).unwrap(), b"incoming");
        let _ = fs::remove_dir_all(&dir);
    }
}
