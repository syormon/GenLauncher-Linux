//! Detecting a mod that ships its own game engine.
//!
//! The launcher never links a mod's `.exe` or `.dll` into the game folder, so
//! a mod normally runs on the game's own engine. Some total conversions are
//! built against a *modified* engine and ship it: their data then uses
//! definitions the stock engine has never heard of, and the game dies while
//! loading with "Technical Difficulties". This module spots that case so the
//! user can be asked which engine to run.
//!
//! The rules, in order:
//!
//! 1. **The mod has an engine.** An executable at the top of the mod's folder
//!    is a game engine when it carries the engine's INI vocabulary. A launcher
//!    stub or an installer does not. World Builder does, since it embeds the
//!    engine, so it is told apart by a marker of its own.
//! 2. **It is not an engine we already have.** A byte-for-byte copy of the
//!    game's or the launcher's engine changes nothing and is ignored.
//! 3. **Evidence that the data needs it.** Every definition name the mod's
//!    INI files use is looked up in both engines; a name only the mod's
//!    engine knows is proof the stock one cannot load this mod. This rule
//!    informs the user rather than gating detection: a custom engine with no
//!    such evidence is still reported.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::game::big;
use crate::util::fs as gfs;

/// INI block names every build of the engine carries as plain strings.
const ENGINE_MARKERS: [&[u8]; 3] = [b"CommandButton", b"ObjectCreationList", b"PlayerTemplate"];

/// Present in World Builder, which embeds the engine, and in no game engine.
const WORLD_BUILDER_MARKER: &[u8] = b"CWorldBuilder";

/// Engines are 6-7 MB; anything far outside that is something else.
const ENGINE_SIZE: std::ops::RangeInclusive<u64> = 1_000_000..=64_000_000;

/// Shorter names are too likely to turn up in a binary by chance.
const MIN_NAME_LEN: usize = 4;

/// A modified game engine found in a mod's folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomEngine {
    /// The engine's file name, at the top of the mod's folder.
    pub file: String,
    /// Definition names the mod's data uses that only this engine understands.
    pub evidence: Vec<String>,
}

/// The mod's own engine, if it ships one that differs from every engine in
/// `known_engines`. Cheap: it reads executables only.
pub fn find(mod_folder: &Path, known_engines: &[PathBuf]) -> Option<String> {
    find_with_contents(mod_folder, known_engines).map(|(file, _)| file)
}

/// [`find`], plus the evidence that the mod's data depends on that engine.
/// Reads every INI file the mod ships, so call it once and keep the answer.
pub fn detect(mod_folder: &Path, known_engines: &[PathBuf]) -> Option<CustomEngine> {
    let (file, engine) = find_with_contents(mod_folder, known_engines)?;

    // Only real engines are a fair comparison: the Steam `generals.exe` is a
    // launcher stub and knows no definitions at all.
    let known: Vec<Vec<u8>> = known_engines
        .iter()
        .filter_map(|path| read_engine(path))
        .collect();

    let evidence = if known.is_empty() {
        Vec::new()
    } else {
        let own = names_in(&engine);
        let standard: Vec<HashSet<&[u8]>> = known.iter().map(|bytes| names_in(bytes)).collect();

        definition_names(mod_folder)
            .into_iter()
            .filter(|name| {
                own.contains(name.as_bytes())
                    && !standard.iter().any(|names| names.contains(name.as_bytes()))
            })
            .collect()
    };

    Some(CustomEngine { file, evidence })
}

fn find_with_contents(mod_folder: &Path, known_engines: &[PathBuf]) -> Option<(String, Vec<u8>)> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(mod_folder)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            gfs::extension_of(p) == "exe" || gfs::file_name_of(p).eq_ignore_ascii_case("game.dat")
        })
        .collect();

    // The names the game itself uses come first; then alphabetically.
    candidates.sort_by_key(|p| {
        let name = gfs::file_name_of(p).to_ascii_lowercase();
        let rank = match name.as_str() {
            "generals.exe" => 0,
            "game.dat" => 1,
            _ => 2,
        };
        (rank, name)
    });

    candidates.into_iter().find_map(|path| {
        let bytes = read_engine(&path)?;
        let is_copy = known_engines.iter().any(|known| same_file_contents(known, &bytes));
        (!is_copy).then(|| (gfs::file_name_of(&path), bytes))
    })
}

/// The file's contents, if it is a game engine.
fn read_engine(path: &Path) -> Option<Vec<u8>> {
    let size = std::fs::metadata(path).ok()?.len();
    if !ENGINE_SIZE.contains(&size) {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    is_game_engine(&bytes).then_some(bytes)
}

fn is_game_engine(bytes: &[u8]) -> bool {
    ENGINE_MARKERS.iter().all(|marker| contains(bytes, marker))
        && !contains(bytes, WORLD_BUILDER_MARKER)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn same_file_contents(path: &Path, bytes: &[u8]) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() == bytes.len() as u64)
        && std::fs::read(path).is_ok_and(|other| other == bytes)
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Every identifier-like run in a binary. The engine stores each definition
/// name it understands as a C string, so those names are all in here.
fn names_in(bytes: &[u8]) -> HashSet<&[u8]> {
    bytes
        .split(|byte| !is_name_byte(*byte))
        .filter(|run| run.len() >= MIN_NAME_LEN && run[0].is_ascii_alphabetic())
        .collect()
}

/// The definition names the mod's INI files use: the first word of each line,
/// which is always a block or field name. Loose files and the mod's archives.
fn definition_names(mod_folder: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();

    let loose = gfs::resolve_case_insensitive(mod_folder, Path::new("Data/INI"));
    gfs::visit_files(&loose, &mut |file| {
        if gfs::extension_of(file) == "ini" {
            if let Ok(text) = std::fs::read(file) {
                collect_names(&text, &mut names);
            }
        }
    });

    for archive in archives_in(mod_folder) {
        for text in ini_files_in_archive(&archive) {
            collect_names(&text, &mut names);
        }
    }

    names
}

/// The mod's archives: `.big`/`.gib` files at the top of its folder, which is
/// the only place the game loads them from. By extension only, since a total
/// conversion can hold thousands of loose files and opening each to check its
/// header takes a minute.
fn archives_in(mod_folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(mod_folder) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| matches!(gfs::extension_of(p).as_str(), "big" | "gib"))
        .collect()
}

fn ini_files_in_archive(archive: &Path) -> Vec<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(entries) = big::read_entries(archive) else { return Vec::new() };
    let Ok(mut file) = std::fs::File::open(archive) else { return Vec::new() };

    entries
        .iter()
        .filter(|entry| {
            let name = entry.name.to_ascii_lowercase().replace('/', "\\");
            name.starts_with("data\\ini\\") && name.ends_with(".ini")
        })
        // A corrupt table could claim an enormous file; no INI is this big.
        .filter(|entry| entry.length <= 64_000_000)
        .filter_map(|entry| {
            let mut text = vec![0u8; entry.length as usize];
            file.seek(SeekFrom::Start(u64::from(entry.offset))).ok()?;
            file.read_exact(&mut text).ok()?;
            Some(text)
        })
        .collect()
}

fn collect_names(ini: &[u8], names: &mut BTreeSet<String>) {
    for line in ini.split(|byte| *byte == b'\n') {
        let code = line.split(|byte| *byte == b';').next().unwrap_or_default();
        let start = code.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(code.len());
        let word = &code[start..];
        let end = word.iter().position(|b| !is_name_byte(*b)).unwrap_or(word.len());
        let word = &word[..end];

        if word.len() >= MIN_NAME_LEN && word[0].is_ascii_alphabetic() {
            names.insert(String::from_utf8_lossy(word).into_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gl-engine-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in engine: the vocabulary markers, the given definition names
    /// as C strings, and padding up to a plausible size.
    fn engine(names: &[&str]) -> Vec<u8> {
        // Filled with a byte that cannot be part of a name, like the NULs
        // and machine code around the strings in a real binary.
        let mut bytes = vec![0x90u8; 1_200_000];
        let mut at = 4096;
        let mut put = |text: &[u8]| {
            bytes[at..at + text.len()].copy_from_slice(text);
            at += text.len() + 1;
        };
        for marker in ENGINE_MARKERS {
            put(marker);
        }
        for name in names {
            put(name.as_bytes());
        }
        bytes
    }

    fn write(path: PathBuf, bytes: &[u8]) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    /// Runs the detector over every installed mod in a real game folder and
    /// prints what it makes of each. Read-only.
    ///
    /// ```text
    /// GENLAUNCHER_TEST_GAME_DIR=<game folder>     ///   cargo test live_scan_of_a_real_mod_store -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real game folder; see the doc comment"]
    fn live_scan_of_a_real_mod_store() {
        let Some(game) = std::env::var_os("GENLAUNCHER_TEST_GAME_DIR").map(PathBuf::from) else {
            eprintln!("skipped: GENLAUNCHER_TEST_GAME_DIR is not set");
            return;
        };

        let mut known = vec![
            gfs::resolve_case_insensitive(&game, Path::new("generals.exe")),
            gfs::resolve_case_insensitive(&game, Path::new("game.dat")),
        ];
        let store = game.join(crate::config::MODS_FOLDER);
        gfs::visit_files(&store.join("Original Game").join("Executables"), &mut |file| {
            if gfs::extension_of(file) == "exe" {
                known.push(file.to_path_buf());
            }
        });
        for engine in &known {
            println!("known: {} (engine: {})", engine.display(), read_engine(engine).is_some());
        }

        let mut versions = Vec::new();
        for modification in fs::read_dir(&store).unwrap().flatten() {
            for version in fs::read_dir(modification.path()).into_iter().flatten().flatten() {
                if version.path().is_dir() {
                    versions.push(version.path());
                }
            }
        }
        versions.sort();

        for folder in versions {
            let started = std::time::Instant::now();
            let found = detect(&folder, &known);
            let label = folder.strip_prefix(&store).unwrap().display().to_string();
            match found {
                Some(engine) => println!(
                    "CUSTOM  {label}: {} | evidence ({}): {:?} [{:.0?}]",
                    engine.file,
                    engine.evidence.len(),
                    engine.evidence.iter().take(12).collect::<Vec<_>>(),
                    started.elapsed()
                ),
                None => println!("-       {label} [{:.0?}]", started.elapsed()),
            }
        }
    }

    const STOCK: &[&str] = &["ChallengeGenerals", "GeneralPersona6", "GeneralPersona7"];
    /// As in the mod that prompted this: slot 7 is spelled with a capital I.
    const MODIFIED: &[&str] = &["ChallengeGenerals", "GeneralPersona6", "GeneraIPersona7"];

    #[test]
    fn a_modified_engine_is_found_with_the_names_only_it_understands() {
        let dir = scratch("modified");
        let stock = write(dir.join("game/game.dat"), &engine(STOCK));
        let mod_folder = dir.join("mod");
        write(mod_folder.join("generals.exe"), &engine(MODIFIED));
        write(
            mod_folder.join("Data/INI/ChallengeMode.ini"),
            b"; comment GeneralPersona99\r\nChallengeGenerals\r\n  GeneralPersona6\r\n    Campaign = x\r\n  End\r\n  GeneraIPersona7 ; note\r\n  End\r\nEnd\r\n",
        );

        let found = detect(&mod_folder, std::slice::from_ref(&stock)).expect("an engine");
        assert_eq!(found.file, "generals.exe");
        assert_eq!(found.evidence, ["GeneraIPersona7"]);
        assert_eq!(find(&mod_folder, &[stock]).as_deref(), Some("generals.exe"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn evidence_is_also_read_from_the_mods_archives() {
        let dir = scratch("archive");
        let stock = write(dir.join("game/game.dat"), &engine(STOCK));
        let mod_folder = dir.join("mod");
        write(mod_folder.join("game.dat"), &engine(MODIFIED));

        // A minimal BIGF archive holding one INI file.
        let name = b"Data\\INI\\ChallengeMode.ini\0";
        let body = b"ChallengeGenerals\n  GeneraIPersona7\n  End\nEnd\n";
        let header_len = 16 + 8 + name.len();
        let mut big = Vec::new();
        big.extend_from_slice(b"BIGF");
        big.extend_from_slice(&((header_len + body.len()) as u32).to_le_bytes());
        big.extend_from_slice(&1u32.to_be_bytes());
        big.extend_from_slice(&(header_len as u32).to_be_bytes());
        big.extend_from_slice(&(header_len as u32).to_be_bytes());
        big.extend_from_slice(&(body.len() as u32).to_be_bytes());
        big.extend_from_slice(name);
        big.extend_from_slice(body);
        write(mod_folder.join("!Mod.gib"), &big);

        let found = detect(&mod_folder, &[stock]).expect("an engine");
        assert_eq!(found.file, "game.dat");
        assert_eq!(found.evidence, ["GeneraIPersona7"]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_copy_of_an_engine_we_already_have_is_not_custom() {
        let dir = scratch("copy");
        let stock = write(dir.join("game/game.dat"), &engine(STOCK));
        let mod_folder = dir.join("mod");
        // Same bytes under the other name, as a full-game repack would have.
        write(mod_folder.join("generals.exe"), &engine(STOCK));

        assert_eq!(detect(&mod_folder, &[stock]), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn launcher_stubs_installers_and_world_builder_are_not_engines() {
        let dir = scratch("others");
        let stock = write(dir.join("game/game.dat"), &engine(STOCK));
        let mod_folder = dir.join("mod");

        // A stub: big enough, but none of the vocabulary.
        write(mod_folder.join("generals.exe"), &vec![0x90u8; 4_300_000]);
        // An installer: too small to be an engine at all.
        write(mod_folder.join("Setup.exe"), b"MZ tiny");
        // World Builder embeds the engine, and says so.
        let mut world_builder = engine(MODIFIED);
        world_builder[2048..2048 + WORLD_BUILDER_MARKER.len()].copy_from_slice(WORLD_BUILDER_MARKER);
        write(mod_folder.join("WorldBuilder.exe"), &world_builder);
        // An engine deeper in the tree is somebody's support file, not the mod's.
        write(mod_folder.join("support/generals.exe"), &engine(MODIFIED));

        assert_eq!(detect(&mod_folder, &[stock]), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_custom_engine_is_reported_even_without_evidence() {
        let dir = scratch("noevidence");
        let stock = write(dir.join("game/game.dat"), &engine(STOCK));
        let mod_folder = dir.join("mod");
        // Different bytes, but the data sticks to names the stock engine knows.
        write(mod_folder.join("Zero Hour.exe"), &engine(&["ChallengeGenerals", "GeneralPersona6", "GeneralPersona7", "Extra"]));
        write(mod_folder.join("Data/INI/ChallengeMode.ini"), b"ChallengeGenerals\n  GeneralPersona7\n  End\nEnd\n");

        let found = detect(&mod_folder, std::slice::from_ref(&stock)).expect("an engine");
        assert_eq!(found.file, "Zero Hour.exe");
        assert!(found.evidence.is_empty(), "unexpected evidence: {:?}", found.evidence);

        // With no standard engine to compare against, there can be no evidence either.
        assert!(detect(&mod_folder, &[]).is_some_and(|e| e.evidence.is_empty()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mod_without_executables_has_no_engine() {
        let dir = scratch("plain");
        write(dir.join("mod/!Mod.gib"), b"BIGF");
        write(dir.join("mod/Data/INI/Object/Thing.ini"), b"Object Thing\nEnd\n");

        assert_eq!(detect(&dir.join("mod"), &[]), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
