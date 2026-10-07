//! Archive extraction for downloaded mods and manually added files.
//!
//! zip and 7z are handled in-process. rar has no pure-Rust reader, so we shell
//! out to 7-Zip, `unrar` or `unar` when one of them is installed — the same
//! formats the WPF build accepted via SevenZipExtractor.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::fs::{self as gfs, extension_of};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    SevenZ,
    Rar,
}

pub fn detect_format(path: &Path) -> Option<Format> {
    match extension_of(path).as_str() {
        "zip" => Some(Format::Zip),
        "7z" => Some(Format::SevenZ),
        "rar" => Some(Format::Rar),
        _ => None,
    }
}

/// True when `path` looks like an archive we can unpack.
pub fn is_supported_archive(path: &Path) -> bool {
    detect_format(path).is_some()
}

/// Unpack `archive` into `dest`, preserving the archive's directory structure.
///
/// When `big_to_gib` is set, every extracted `*.big` is renamed to `*.gib` —
/// the launcher keeps mod archives under that extension so the game does not
/// pick them up until they are linked in.
pub fn extract(archive: &Path, dest: &Path, big_to_gib: bool) -> Result<Vec<PathBuf>> {
    extract_with_progress(archive, dest, big_to_gib, &|_| {})
}

/// [`extract`], reporting how far it has got: `Some(0.0..=1.0)`, or `None`
/// while there is no figure to give. zip and 7z count the bytes they write;
/// rar relays the percentage its external tool prints, if it prints one.
pub fn extract_with_progress(
    archive: &Path,
    dest: &Path,
    big_to_gib: bool,
    progress: &dyn Fn(Option<f32>),
) -> Result<Vec<PathBuf>> {
    let format = detect_format(archive)
        .with_context(|| format!("unsupported archive type: {}", archive.display()))?;

    fs::create_dir_all(dest)?;

    // Unpack to a sibling staging directory first so a failure cannot leave
    // half-renamed files in the destination.
    let staging = dest.join(".gl-extract-tmp");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;

    progress(None);
    let result = match format {
        Format::Zip => extract_zip(archive, &staging, progress),
        Format::SevenZ => extract_7z(archive, &staging, progress),
        Format::Rar => extract_rar_external(archive, &staging, progress),
    };

    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    let mut written = Vec::new();
    move_tree(&staging, dest, big_to_gib, &mut written)?;
    let _ = fs::remove_dir_all(&staging);

    Ok(written)
}

/// Passes reads through, telling `on_total` the running byte count.
struct CountingReader<'a, F: FnMut(u64)> {
    inner: &'a mut dyn Read,
    total: &'a mut u64,
    on_total: F,
}

impl<F: FnMut(u64)> Read for CountingReader<'_, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        *self.total += read as u64;
        (self.on_total)(*self.total);
        Ok(read)
    }
}

fn fraction(done: u64, total: u64) -> Option<f32> {
    (total > 0).then(|| (done as f64 / total as f64).min(1.0) as f32)
}

fn extract_zip(archive: &Path, dest: &Path, progress: &dyn Fn(Option<f32>)) -> Result<()> {
    let file = fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let total = zip.decompressed_size().map(|size| size as u64).unwrap_or(0);
    let mut done = 0u64;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // `enclosed_name` rejects `..` and absolute paths (zip-slip).
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut writer = fs::File::create(&out)?;
        let mut counting = CountingReader {
            inner: &mut entry,
            total: &mut done,
            on_total: |done| progress(fraction(done, total)),
        };
        std::io::copy(&mut counting, &mut writer)?;
    }
    Ok(())
}

fn extract_7z(archive: &Path, dest: &Path, progress: &dyn Fn(Option<f32>)) -> Result<()> {
    // Without the total there is still an archive to unpack, just no figure.
    let total = sevenz_rust2::Archive::open(archive)
        .map(|a| a.files.iter().map(|f| f.size()).sum::<u64>())
        .unwrap_or(0);
    let mut done = 0u64;

    sevenz_rust2::decompress_file_with_extract_fn(archive, dest, |entry, reader, path| {
        let mut counting = CountingReader {
            inner: reader,
            total: &mut done,
            on_total: |done| progress(fraction(done, total)),
        };
        sevenz_rust2::default_entry_extract_fn(entry, &mut counting, path)
    })
    .map_err(|e| anyhow::anyhow!("7z extraction failed: {e}"))
}

/// A program that can unpack RAR archives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RarTool {
    pub program: PathBuf,
    kind: RarToolKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RarToolKind {
    SevenZip,
    Unrar,
    Unar,
}

/// Every RAR-capable tool on this machine, best first: on PATH, then (on
/// Windows) in the folders the 7-Zip and WinRAR installers use, since neither
/// installer puts itself on PATH.
///
/// `7za` and `7zr` are left out on purpose: those reduced builds cannot read RAR.
pub fn rar_tools() -> Vec<RarTool> {
    let mut tools = Vec::new();
    let mut add = |program: PathBuf, kind| {
        if !tools.iter().any(|t: &RarTool| t.program == program) {
            tools.push(RarTool { program, kind });
        }
    };

    for (name, kind) in [
        ("7z", RarToolKind::SevenZip),
        ("7zz", RarToolKind::SevenZip),
        ("unrar", RarToolKind::Unrar),
        ("unar", RarToolKind::Unar),
    ] {
        if let Some(program) = find_on_path(name) {
            add(program, kind);
        }
    }

    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            let Some(base) = std::env::var_os(var).map(PathBuf::from) else { continue };
            for (rel, kind) in [
                ("7-Zip/7z.exe", RarToolKind::SevenZip),
                ("WinRAR/UnRAR.exe", RarToolKind::Unrar),
            ] {
                let program = base.join(rel);
                if program.is_file() {
                    add(program, kind);
                }
            }
        }
    }

    tools
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(&file)).find(|p| p.is_file())
}

/// True when `path` can be unpacked on this machine right now. zip and 7z
/// always can; rar needs one of [`rar_tools`].
pub fn can_extract(path: &Path) -> bool {
    match detect_format(path) {
        Some(Format::Rar) => !rar_tools().is_empty(),
        Some(_) => true,
        None => false,
    }
}

impl RarTool {
    fn command(&self, archive: &Path, dest: &Path) -> Command {
        let mut cmd = Command::new(&self.program);
        match self.kind {
            RarToolKind::SevenZip => {
                // -bsp1: print the running percentage to stdout, where it is
                // read for the progress bar. Off by default when not a console.
                cmd.arg("x")
                    .arg("-y")
                    .arg("-bsp1")
                    .arg(format!("-o{}", dest.display()))
                    .arg(archive);
            }
            RarToolKind::Unrar => {
                // unrar takes the destination as a trailing folder argument.
                cmd.arg("x").arg("-y").arg(archive).arg(dest.join(""));
            }
            RarToolKind::Unar => {
                // -D: do not wrap the contents in a folder named after the archive.
                cmd.arg("-f").arg("-D").arg("-o").arg(dest).arg(archive);
            }
        }
        cmd.stdin(std::process::Stdio::null());

        // The launcher is a GUI program; do not flash a console for the tool.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }
}

fn extract_rar_external(archive: &Path, dest: &Path, progress: &dyn Fn(Option<f32>)) -> Result<()> {
    let tools = rar_tools();
    if tools.is_empty() {
        bail!("RAR archives need 7-Zip or UnRAR to be installed; neither was found");
    }

    let mut failures = Vec::new();
    for tool in &tools {
        // A 7-Zip without its RAR codec (Linux `p7zip` without `p7zip-rar`)
        // fails here; the next tool may still work.
        match tool.run(archive, dest, progress) {
            Ok(()) => return Ok(()),
            Err(reason) => failures.push(format!("{}: {reason}", tool.program.display())),
        }
        progress(None);
    }

    bail!("could not unpack the RAR archive ({})", failures.join("; "))
}

impl RarTool {
    /// Run the tool to completion, relaying the percentages it prints.
    /// On failure, the last thing it said on stderr.
    fn run(&self, archive: &Path, dest: &Path, progress: &dyn Fn(Option<f32>)) -> Result<(), String> {
        use std::process::Stdio;

        let mut child = self
            .command(archive, dest)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;

        // Drained on its own thread: a tool blocked writing to a full stderr
        // pipe would never finish writing to stdout either.
        let mut stderr = child.stderr.take().expect("stderr was piped");
        let errors = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        });

        let mut stdout = child.stdout.take().expect("stdout was piped");
        let mut percent = PercentReader::new(self.kind);
        let mut chunk = [0u8; 4096];
        while let Ok(read) = stdout.read(&mut chunk) {
            if read == 0 {
                break;
            }
            if let Some(value) = percent.feed(&chunk[..read]) {
                progress(Some(f32::from(value) / 100.0));
            }
        }

        let status = child.wait().map_err(|e| e.to_string())?;
        let errors = errors.join().unwrap_or_default();
        if status.success() {
            return Ok(());
        }
        let reason = errors.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        Err(if reason.is_empty() { status.to_string() } else { reason.trim().to_owned() })
    }
}

/// Picks the running percentage out of an extraction tool's output.
///
/// 7-Zip rewrites one line with carriage returns: `\r 42% 3 - file.big`.
/// UnRAR backspaces over its last figure: `\x08\x08\x08\x08 42%`. A number
/// counts only straight after that marker, so `50% off.big` in a file name
/// is not taken for progress. Figures never go backwards.
struct PercentReader {
    /// The byte that announces a percentage.
    marker: fn(u8) -> bool,
    /// Just past a marker, with only spaces since.
    armed: bool,
    digits: u8,
    value: u32,
    highest: Option<u8>,
}

impl PercentReader {
    fn new(kind: RarToolKind) -> Self {
        let marker: fn(u8) -> bool = match kind {
            RarToolKind::Unrar => |byte| byte == 0x08,
            // `unar` prints no percentages; this simply never matches much.
            RarToolKind::SevenZip | RarToolKind::Unar => |byte| byte == b'\r' || byte == b'\n',
        };
        Self { marker, armed: true, digits: 0, value: 0, highest: None }
    }

    /// Feed the next piece of output; returns a new, higher percentage if
    /// this piece contained one.
    fn feed(&mut self, chunk: &[u8]) -> Option<u8> {
        let mut found = None;
        for &byte in chunk {
            match byte {
                b'0'..=b'9' if (self.armed || self.digits > 0) && self.digits < 3 => {
                    self.value = self.value * 10 + u32::from(byte - b'0');
                    self.digits += 1;
                    self.armed = false;
                }
                b'%' if self.digits > 0 && self.value <= 100 => {
                    let value = self.value as u8;
                    if self.highest.is_none_or(|highest| value > highest) {
                        self.highest = Some(value);
                        found = Some(value);
                    }
                    self.digits = 0;
                    self.value = 0;
                }
                // Spaces pad the figure; they neither arm nor disarm.
                b' ' if self.digits == 0 => {}
                _ => {
                    self.digits = 0;
                    self.value = 0;
                    self.armed = (self.marker)(byte);
                }
            }
        }
        found
    }
}

/// Move everything from `from` into `to`, applying the .big to .gib rename.
fn move_tree(from: &Path, to: &Path, big_to_gib: bool, written: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let src = entry.path();

        if entry.file_type()?.is_dir() {
            let sub = to.join(&name);
            fs::create_dir_all(&sub)?;
            move_tree(&src, &sub, big_to_gib, written)?;
            continue;
        }

        let mut target = to.join(&name);
        if big_to_gib && extension_of(&src) == "big" {
            target = gfs::change_extension(&target, "gib");
        }
        if target.exists() {
            let _ = fs::remove_file(&target);
        }
        gfs::move_file(&src, &target)?;
        written.push(target);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_formats() {
        assert_eq!(detect_format(Path::new("a/ROTR.7z")), Some(Format::SevenZ));
        assert_eq!(detect_format(Path::new("a/ROTR.ZIP")), Some(Format::Zip));
        assert_eq!(detect_format(Path::new("a/ROTR.big")), None);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gl-archive-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Data that does not compress away, so progress has something to count.
    fn noise(len: usize, seed: u32) -> Vec<u8> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect()
    }

    /// Unpack `archive` and return every fraction reported along the way.
    fn fractions_while_extracting(archive: &Path, dest: &Path) -> Vec<Option<f32>> {
        let seen = std::cell::RefCell::new(Vec::new());
        extract_with_progress(archive, dest, true, &|fraction| seen.borrow_mut().push(fraction))
            .unwrap();
        seen.into_inner()
    }

    fn assert_runs_from_nothing_to_complete(seen: &[Option<f32>]) {
        // Unknown first, so the bar can show activity before the first figure.
        assert_eq!(seen.first(), Some(&None));
        let figures: Vec<f32> = seen.iter().flatten().copied().collect();
        assert!(figures.len() >= 3, "too few progress reports: {figures:?}");
        assert!(figures.windows(2).all(|pair| pair[0] <= pair[1]), "progress went backwards");
        assert!(figures[0] < 0.5, "the first report was already {}", figures[0]);
        assert_eq!(figures.last().copied(), Some(1.0));
    }

    #[test]
    fn a_zip_reports_progress_by_bytes_written() {
        use std::io::Write;

        let dir = scratch("zip");
        let archive = dir.join("mod.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("!Mod.big", options).unwrap();
        zip.write_all(&noise(300_000, 1)).unwrap();
        zip.start_file("Data/INI/Object.ini", options).unwrap();
        zip.write_all(&noise(100_000, 2)).unwrap();
        zip.finish().unwrap();

        let out = dir.join("out");
        let seen = fractions_while_extracting(&archive, &out);

        assert_runs_from_nothing_to_complete(&seen);
        // The files arrive whole, with the archive stored under `.gib`.
        assert_eq!(fs::read(out.join("!Mod.gib")).unwrap(), noise(300_000, 1));
        assert_eq!(fs::read(out.join("Data/INI/Object.ini")).unwrap(), noise(100_000, 2));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_7z_reports_progress_by_bytes_written() {
        let dir = scratch("7z");
        let source = dir.join("source");
        fs::create_dir_all(source.join("Data")).unwrap();
        fs::write(source.join("!Mod.big"), noise(300_000, 3)).unwrap();
        fs::write(source.join("Data/Thing.ini"), noise(100_000, 4)).unwrap();
        let archive = dir.join("mod.7z");
        sevenz_rust2::compress_to_path(&source, &archive).unwrap();

        let out = dir.join("out");
        let seen = fractions_while_extracting(&archive, &out);

        assert_runs_from_nothing_to_complete(&seen);
        assert_eq!(fs::read(out.join("!Mod.gib")).unwrap(), noise(300_000, 3));
        assert_eq!(fs::read(out.join("Data/Thing.ini")).unwrap(), noise(100_000, 4));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Runs the first installed RAR tool on a real archive and prints the
    /// progress it relays. 7-Zip unpacks any format the same way, so a zip
    /// will do when no RAR is at hand.
    ///
    /// ```text
    /// GENLAUNCHER_TEST_ARCHIVE=<archive> GENLAUNCHER_TEST_OUT=<empty scratch folder> \
    ///   cargo test live_external_tool_reports_progress -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs an archive and an installed 7-Zip or UnRAR; see the doc comment"]
    fn live_external_tool_reports_progress() {
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let (Some(archive), Some(out)) = (var("GENLAUNCHER_TEST_ARCHIVE"), var("GENLAUNCHER_TEST_OUT")) else {
            eprintln!("skipped: the GENLAUNCHER_TEST_* variables are not set");
            return;
        };
        let tool = rar_tools().into_iter().next().expect("no RAR tool is installed");
        println!("tool: {}", tool.program.display());
        fs::create_dir_all(&out).unwrap();

        let started = std::time::Instant::now();
        let seen = std::cell::RefCell::new(Vec::new());
        tool.run(&archive, &out, &|fraction| {
            println!("at {:>5.1?}: {:?}", started.elapsed(), fraction.map(|f| (f * 100.0).round()));
            seen.borrow_mut().push(fraction);
        })
        .expect("the tool failed");

        let figures: Vec<f32> = seen.into_inner().into_iter().flatten().collect();
        assert!(figures.len() >= 2, "the tool reported no progress: {figures:?}");
        assert!(figures.windows(2).all(|pair| pair[0] < pair[1]), "not increasing: {figures:?}");
    }

    /// Feed `output` in pieces of `step` bytes and collect what is reported:
    /// the newest percentage in each piece, as a progress bar wants.
    fn percentages(kind: RarToolKind, output: &[u8], step: usize) -> Vec<u8> {
        let mut reader = PercentReader::new(kind);
        output.chunks(step).filter_map(|chunk| reader.feed(chunk)).collect()
    }

    #[test]
    fn reads_the_percentages_7zip_prints() {
        // Captured from `7z x -bsp1` (24.06) unpacking a real mod archive.
        let output = b"\r\n7-Zip 24.06 (x64) : Copyright (c) 1999-2024 Igor Pavlov : 2024-05-26\r\n\r\n\
            Scanning the drive for archives:\r\n  0M Scan D:\\Games\\\r                    \r\
            1 file, 366438649 bytes (350 MiB)\r\n\r\nExtracting archive: DeepImpact_BetaV1.zip\r\n--\r\n\
            Path = DeepImpact_BetaV1.zip\r\nType = zip\r\nPhysical Size = 366438649\r\n\r\n\
            \x20 0%\r    \r  1% - !DImpact_Window.big\r                          \r\
            \x20 7% 1 - !DImpact_Art.big\r                         \r 42% 1 - !DImpact_Art.big\r   \r\
            \x2094% 2 - !DImpact_Audio.big\r                           \rEverything is Ok\r\n\r\n\
            Folders: 2\r\nFiles: 12\r\nSize:       720178957\r\nCompressed: 366438649\r\n";

        // Every figure is picked up, however the pipe happens to split the stream.
        assert_eq!(percentages(RarToolKind::SevenZip, output, 1), [0, 1, 7, 42, 94]);
        assert_eq!(percentages(RarToolKind::SevenZip, output, 7), [0, 1, 7, 42, 94]);
        // Arriving all at once, only the newest matters.
        assert_eq!(percentages(RarToolKind::SevenZip, output, 4096), [94]);
    }

    #[test]
    fn a_percent_sign_in_a_file_name_is_not_progress() {
        let output = b"\r 10% 1 - 50% off sale.big\r    \r 20% 2 - 100% orange.big\r";
        assert_eq!(percentages(RarToolKind::SevenZip, output, 1), [10, 20]);
    }

    #[test]
    fn reads_the_percentages_unrar_prints() {
        // UnRAR backspaces over its previous figure rather than restarting the line.
        let output = b"Extracting from mod.rar\n\nExtracting  75% done.big      \x08\x08\x08\x08  5%\
            \x08\x08\x08\x08 40%\x08\x08\x08\x08 99%\x08\x08\x08\x08\x08  OK \nAll OK\n";
        assert_eq!(percentages(RarToolKind::Unrar, output, 1), [5, 40, 99]);
        assert_eq!(percentages(RarToolKind::Unrar, output, 3), [5, 40, 99]);
    }

    #[test]
    fn zip_and_7z_never_need_an_external_tool() {
        assert!(can_extract(Path::new("mod.zip")));
        assert!(can_extract(Path::new("mod.7z")));
        assert!(!can_extract(Path::new("mod.big")));
        // rar depends on what is installed, which is exactly what it reports.
        assert_eq!(can_extract(Path::new("mod.rar")), !rar_tools().is_empty());
    }
}
