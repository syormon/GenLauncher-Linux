//! GE-Proton and the Steam Linux Runtime, downloaded and kept by the launcher,
//! so the game runs through Proton without Steam.
//!
//! A small built-in take on what umu-launcher does, written from Valve's
//! published interfaces (the `STEAM_COMPAT_*` variables, `_v2-entry-point`,
//! `toolmanifest.vdf`) rather than from umu's code, as in bfme-launcher:
//!
//! 1. GE-Proton comes from its GitHub releases, SHA-512 checked, into
//!    `runtime/proton/<version>`.
//! 2. Its `toolmanifest.vdf` names the Steam Linux Runtime it was built for,
//!    which comes from repo.steampowered.com, SHA-256 checked, into
//!    `runtime/steamrt-<appid>`.
//! 3. A copy of the game outside Steam runs in the launcher's own prefix,
//!    `prefix/pfx`.
//!
//! All of it lives in the user's data folder (`~/.local/share/genlauncher`):
//! outside the game folder, so it is shared by every game folder and never
//! taken for game files.
#![cfg_attr(windows, allow(dead_code))]

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use crate::net::{download, CLIENT};

/// How many times a download is tried before giving up.
const DOWNLOAD_ATTEMPTS: u32 = 3;

/// What to report while downloading or unpacking: a label, and for a download
/// its size (when known) and the bytes so far. Unpacking reports no size.
pub type Progress<'a> = &'a dyn Fn(&str, Option<u64>, u64);

/// A Steam Linux Runtime a Proton build can ask for (`require_tool_appid`).
#[derive(Debug)]
pub struct RuntimeSpec {
    pub appid: &'static str,
    pub name: &'static str,
    /// Its folder on the Steam runtime server.
    path: &'static str,
    archive: &'static str,
}

pub const RUNTIMES: [RuntimeSpec; 2] = [
    RuntimeSpec {
        appid: "1628350",
        name: "Steam Linux Runtime 3.0 (sniper)",
        path: "steamrt-images-sniper/snapshots/latest-container-runtime-depot/",
        archive: "SteamLinuxRuntime_sniper.tar.xz",
    },
    RuntimeSpec {
        appid: "4183110",
        name: "Steam Linux Runtime 4",
        path: "steamrt4/images/latest-container-runtime-depot/",
        archive: "SteamLinuxRuntime_4.tar.xz",
    },
];

pub fn runtime_spec(appid: &str) -> Option<&'static RuntimeSpec> {
    RUNTIMES.iter().find(|r| r.appid == appid)
}

/// The runtime a Proton build asks for in its `toolmanifest.vdf`, by app id.
pub fn required_runtime(proton_dir: &Path) -> Option<String> {
    let manifest = std::fs::read_to_string(proton_dir.join("toolmanifest.vdf")).ok()?;
    crate::game::steam::vdf_value(&manifest, "require_tool_appid")
}

/// Where the downloads come from. Tests point these at a local server.
#[derive(Debug, Clone)]
pub struct Sources {
    /// GitHub's "latest release" API for GE-Proton.
    pub ge_proton_latest: String,
    /// The Steam runtime server, ending in `/`.
    pub steam_runtimes: String,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            ge_proton_latest: "https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases/latest"
                .to_owned(),
            steam_runtimes: "https://repo.steampowered.com/".to_owned(),
        }
    }
}

/// What has been installed, kept in `runtime/state.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// The managed GE-Proton: its folder name under `runtime/proton`.
    proton: String,
    /// Runtime app id → the `VERSION.txt` installed.
    runtimes: BTreeMap<String, String>,
}

/// The launcher's own Proton, runtime and prefix.
#[derive(Debug, Clone)]
pub struct Managed {
    root: PathBuf,
    sources: Sources,
}

impl Default for Managed {
    fn default() -> Self {
        let data = dirs::data_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self { root: data.join("genlauncher"), sources: Sources::default() }
    }
}

impl Managed {
    /// Somewhere else, from somewhere else: for tests.
    #[cfg(test)]
    pub fn new(root: PathBuf, sources: Sources) -> Self {
        Self { root, sources }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `STEAM_COMPAT_DATA_PATH` for a game outside Steam: the prefix is `pfx`
    /// inside it.
    pub fn compat_data(&self) -> PathBuf {
        self.root.join("prefix")
    }

    /// Stands in for a Steam client, which Proton only looks in for Steam's
    /// own libraries.
    pub fn steam_client_stub(&self) -> PathBuf {
        self.root.join("steam")
    }

    fn runtime_root(&self) -> PathBuf {
        self.root.join("runtime")
    }

    /// The downloaded GE-Proton's folder, if there is one.
    pub fn proton_dir(&self) -> Option<PathBuf> {
        let state = self.state();
        let dir = self.runtime_root().join("proton").join(&state.proton);
        (!state.proton.is_empty() && dir.join("proton").is_file()).then_some(dir)
    }

    /// The downloaded GE-Proton's version, e.g. `GE-Proton10-25`.
    pub fn version(&self) -> Option<String> {
        self.proton_dir().map(|_| self.state().proton)
    }

    /// The `_v2-entry-point` of a downloaded runtime.
    pub fn runtime_entry(&self, appid: &str) -> Option<PathBuf> {
        let entry = self.runtime_dir(appid).join("_v2-entry-point");
        entry.is_file().then_some(entry)
    }

    fn runtime_dir(&self, appid: &str) -> PathBuf {
        self.runtime_root().join(format!("steamrt-{appid}"))
    }

    fn state(&self) -> State {
        std::fs::read_to_string(self.runtime_root().join("state.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, state: &State) -> Result<()> {
        std::fs::create_dir_all(self.runtime_root())?;
        std::fs::write(self.runtime_root().join("state.json"), serde_json::to_string_pretty(state)?)?;
        Ok(())
    }

    // -- installing -------------------------------------------------------

    /// The GE-Proton to run, downloading the latest if there is none yet. With
    /// `update`, also replaces one that is no longer the latest.
    pub fn install_proton(&self, update: bool, progress: Progress) -> Result<PathBuf> {
        if let Some(dir) = self.proton_dir().filter(|_| !update) {
            return Ok(dir);
        }
        let runtime = blocking_runtime()?;

        progress("Looking up the latest GE-Proton", None, 0);
        let json = runtime.block_on(get_text(&self.sources.ge_proton_latest, true))?;
        let release = pick_release(&serde_json::from_str(&json).context("reading GE-Proton's release")?)?;

        let mut state = self.state();
        if state.proton == release.tag {
            if let Some(dir) = self.proton_dir() {
                return Ok(dir);
            }
        }

        let sums = runtime.block_on(get_text(&release.checksum_url, false))?;
        let expected = first_word(&sums);
        let label = format!("Downloading {}", release.tag);
        let file = self.fetch(&runtime, &release.url, &label, progress)?;
        verify(&file, &expected, Hash::Sha512)?;

        progress(&format!("Unpacking {}", release.tag), None, 0);
        let dest = self.runtime_root().join("proton").join(&release.tag);
        extract(&file, &dest, Compression::Gzip)?;
        let _ = std::fs::remove_file(&file);
        anyhow::ensure!(dest.join("proton").is_file(), "{} has no `proton` script", release.tag);

        // Only the version in use is kept.
        let old = std::mem::replace(&mut state.proton, release.tag);
        self.save_state(&state)?;
        if !old.is_empty() && old != state.proton {
            let _ = std::fs::remove_dir_all(self.runtime_root().join("proton").join(old));
        }
        Ok(dest)
    }

    /// The runtime `appid`'s entry point, downloading it if it is missing.
    /// With `update`, also replaces one the server has a newer version of.
    pub fn install_runtime(&self, appid: &str, update: bool, progress: Progress) -> Result<PathBuf> {
        if let Some(entry) = self.runtime_entry(appid).filter(|_| !update) {
            return Ok(entry);
        }
        let spec = runtime_spec(appid)
            .ok_or_else(|| anyhow!("Proton needs Steam runtime {appid}, which the launcher cannot download"))?;
        let base = format!("{}{}", self.sources.steam_runtimes, spec.path);
        let runtime = blocking_runtime()?;

        progress(&format!("Checking {}", spec.name), None, 0);
        let version = runtime.block_on(get_text(&format!("{base}VERSION.txt"), false))?.trim().to_owned();
        let mut state = self.state();
        if let Some(entry) = self.runtime_entry(appid) {
            if state.runtimes.get(appid) == Some(&version) {
                return Ok(entry);
            }
        }

        let sums = runtime.block_on(get_text(&format!("{base}SHA256SUMS"), false))?;
        let expected = checksum_for(&sums, spec.archive)
            .ok_or_else(|| anyhow!("the Steam runtime server lists no checksum for {}", spec.archive))?;
        let file = self.fetch(&runtime, &format!("{base}{}", spec.archive), &format!("Downloading {}", spec.name), progress)?;
        verify(&file, &expected, Hash::Sha256)?;

        progress(&format!("Unpacking {}", spec.name), None, 0);
        let dir = self.runtime_dir(appid);
        extract(&file, &dir, Compression::Xz)?;
        let _ = std::fs::remove_file(&file);
        let entry = self.runtime_entry(appid).ok_or_else(|| anyhow!("{} has no _v2-entry-point", spec.name))?;

        state.runtimes.insert(appid.to_owned(), version);
        self.save_state(&state)?;
        Ok(entry)
    }

    /// Download `url` into `runtime/downloads`, trying a few times.
    fn fetch(&self, runtime: &tokio::runtime::Runtime, url: &str, label: &str, progress: Progress) -> Result<PathBuf> {
        let folder = self.runtime_root().join("downloads");
        let mut last_error = None;
        for attempt in 1..=DOWNLOAD_ATTEMPTS {
            let result = runtime.block_on(download::simple_file(url, &folder, false, |total, read| {
                progress(label, total, read);
            }));
            match result {
                Ok(file) => return Ok(file),
                Err(e) => {
                    log::warn!("{label}, attempt {attempt} of {DOWNLOAD_ATTEMPTS}: {e:#}");
                    last_error = Some(e);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("nothing was downloaded"))).with_context(|| format!("downloading {url}"))
    }
}

fn blocking_runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?)
}

async fn get_text(url: &str, github_api: bool) -> Result<String> {
    let mut request = CLIENT.get(url).timeout(std::time::Duration::from_secs(60));
    if github_api {
        request = request.header(reqwest::header::ACCEPT, "application/vnd.github+json");
    }
    let response = request.send().await?.error_for_status().with_context(|| format!("fetching {url}"))?;
    Ok(response.text().await?)
}

// ---------------------------------------------------------------------------
// Release and checksum listings
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
struct Release {
    tag: String,
    url: String,
    checksum_url: String,
}

/// The x86-64 archive in a GE-Proton release, and its `.sha512sum`.
fn pick_release(release: &serde_json::Value) -> Result<Release> {
    let tag = release["tag_name"].as_str().ok_or_else(|| anyhow!("GE-Proton's release has no tag"))?.to_owned();
    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    let asset = |wanted: &dyn Fn(&str) -> bool| {
        assets.iter().find_map(|a| {
            let name = a["name"].as_str()?;
            wanted(name).then(|| Some((name.to_owned(), a["browser_download_url"].as_str()?.to_owned())))?
        })
    };

    // Newer releases name the archive `<tag>-x86_64.tar.gz`, older ones `<tag>.tar.gz`.
    let (archive, url) = asset(&|n| n.ends_with("-x86_64.tar.gz"))
        .or_else(|| asset(&|n| n.ends_with(".tar.gz") && !n.contains("aarch64") && !n.contains("arm64")))
        .ok_or_else(|| anyhow!("{tag} has no x86-64 download"))?;
    let stem = archive.trim_end_matches(".tar.gz").to_owned();
    let (_, checksum_url) =
        asset(&|n| n == format!("{stem}.sha512sum")).ok_or_else(|| anyhow!("{tag} has no checksum file"))?;
    Ok(Release { tag, url, checksum_url })
}

/// The checksum `SHA256SUMS` lists for `archive`, in either listing style.
fn checksum_for(sums: &str, archive: &str) -> Option<String> {
    sums.lines()
        .map(str::trim_end)
        .find(|line| line.ends_with(&format!(" *{archive}")) || line.ends_with(&format!("  {archive}")))
        .map(first_word)
}

fn first_word(text: &str) -> String {
    text.split_whitespace().next().unwrap_or("").to_ascii_lowercase()
}

// ---------------------------------------------------------------------------
// Checking and unpacking
// ---------------------------------------------------------------------------

enum Hash {
    Sha256,
    Sha512,
}

/// Check `file` against `expected`; a file that does not match is deleted.
fn verify(file: &Path, expected: &str, hash: Hash) -> Result<()> {
    use sha2::Digest;

    fn digest<D: Digest>(mut reader: impl Read) -> Result<String> {
        let mut hasher = D::new();
        let mut buffer = vec![0u8; 1 << 20];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    let reader = BufReader::new(std::fs::File::open(file)?);
    let actual = match hash {
        Hash::Sha256 => digest::<sha2::Sha256>(reader)?,
        Hash::Sha512 => digest::<sha2::Sha512>(reader)?,
    };
    if actual != expected {
        let _ = std::fs::remove_file(file);
        bail!("{} did not download correctly (its checksum does not match). Try again.", file.display());
    }
    Ok(())
}

enum Compression {
    Gzip,
    Xz,
}

/// Unpack `archive` so that its single top-level folder becomes `dest`,
/// replacing it. Unpacked beside `dest` first, so a failure leaves no
/// half-written folder in its place.
fn extract(archive: &Path, dest: &Path, compression: Compression) -> Result<()> {
    let staging = dest.with_extension("partial");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;

    let file = BufReader::new(std::fs::File::open(archive)?);
    let reader: Box<dyn Read> = match compression {
        Compression::Gzip => Box::new(flate2::read::GzDecoder::new(file)),
        Compression::Xz => Box::new(lzma_rust2::XzReader::new(file, true)),
    };
    let mut tar = tar::Archive::new(reader);
    tar.set_preserve_permissions(true);
    tar.set_preserve_mtime(true);
    tar.set_overwrite(true);
    tar.unpack(&staging).with_context(|| format!("unpacking {}", archive.display()))?;

    let entries: Vec<PathBuf> = std::fs::read_dir(&staging)?.flatten().map(|e| e.path()).collect();
    let content = match entries.as_slice() {
        [only] if only.is_dir() => only.clone(),
        _ => staging.clone(),
    };
    let _ = std::fs::remove_dir_all(dest);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&content, dest)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gl-managed-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn picks_the_x86_64_archive_and_its_checksum() {
        let release = serde_json::json!({
            "tag_name": "GE-Proton10-25",
            "assets": [
                { "name": "GE-Proton10-25-aarch64.tar.gz", "browser_download_url": "https://x/arm.tar.gz" },
                { "name": "GE-Proton10-25-x86_64.sha512sum", "browser_download_url": "https://x/sum" },
                { "name": "GE-Proton10-25-x86_64.tar.gz", "browser_download_url": "https://x/x86.tar.gz" },
            ]
        });
        assert_eq!(
            pick_release(&release).unwrap(),
            Release {
                tag: "GE-Proton10-25".into(),
                url: "https://x/x86.tar.gz".into(),
                checksum_url: "https://x/sum".into(),
            }
        );

        // Older releases have no architecture in the name.
        let older = serde_json::json!({
            "tag_name": "GE-Proton9-1",
            "assets": [
                { "name": "GE-Proton9-1.tar.gz", "browser_download_url": "https://x/old.tar.gz" },
                { "name": "GE-Proton9-1.sha512sum", "browser_download_url": "https://x/old.sum" },
            ]
        });
        assert_eq!(pick_release(&older).unwrap().url, "https://x/old.tar.gz");

        let unchecked = serde_json::json!({
            "tag_name": "GE-Proton9-2",
            "assets": [{ "name": "GE-Proton9-2.tar.gz", "browser_download_url": "https://x/a" }]
        });
        assert!(pick_release(&unchecked).is_err(), "an archive without a checksum is not used");
    }

    #[test]
    fn finds_an_archives_checksum_in_either_listing_style() {
        let sums = "AAA *SteamLinuxRuntime_sniper.tar.xz\nbbb  SteamLinuxRuntime_4.tar.xz\nccc  other.tar.xz\n";
        assert_eq!(checksum_for(sums, "SteamLinuxRuntime_sniper.tar.xz").as_deref(), Some("aaa"));
        assert_eq!(checksum_for(sums, "SteamLinuxRuntime_4.tar.xz").as_deref(), Some("bbb"));
        assert_eq!(checksum_for(sums, "missing.tar.xz"), None);
    }

    #[test]
    fn reads_the_runtime_a_proton_build_needs() {
        let dir = scratch("manifest");
        fs::write(
            dir.join("toolmanifest.vdf"),
            "\"manifest\"\n{\n  \"version\" \"2\"\n  \"require_tool_appid\" \"1628350\"\n}\n",
        )
        .unwrap();
        assert_eq!(required_runtime(&dir).as_deref(), Some("1628350"));
        assert_eq!(runtime_spec("1628350").unwrap().archive, "SteamLinuxRuntime_sniper.tar.xz");
        assert!(runtime_spec("1").is_none());

        fs::remove_file(dir.join("toolmanifest.vdf")).unwrap();
        assert_eq!(required_runtime(&dir), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_download_that_does_not_match_its_checksum_is_thrown_away() {
        use sha2::Digest;
        let dir = scratch("verify");
        let file = dir.join("archive");
        fs::write(&file, b"contents").unwrap();
        let sha256 = hex::encode(sha2::Sha256::digest(b"contents"));
        let sha512 = hex::encode(sha2::Sha512::digest(b"contents"));

        verify(&file, &sha256, Hash::Sha256).unwrap();
        verify(&file, &sha512, Hash::Sha512).unwrap();
        assert!(verify(&file, &sha256, Hash::Sha512).is_err());
        assert!(!file.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A `.tar` of `files`, each under `top/`.
    pub fn tarball(top: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, format!("{top}/{name}"), *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    pub fn gzip(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    pub fn xz(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut writer = lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
        writer.write_all(data).unwrap();
        writer.finish().unwrap()
    }

    #[test]
    fn unpacks_an_archives_top_folder_into_place() {
        let dir = scratch("extract");
        let dest = dir.join("runtime/proton/GE-Proton10-25");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("stale"), b"from before").unwrap();

        for (compression, bytes) in [
            (Compression::Gzip, gzip(&tarball("GE-Proton10-25", &[("proton", b"#!/bin/sh"), ("files/lib", b"x")]))),
            (Compression::Xz, xz(&tarball("GE-Proton10-25", &[("proton", b"#!/bin/sh"), ("files/lib", b"x")]))),
        ] {
            let archive = dir.join("archive");
            fs::write(&archive, bytes).unwrap();
            extract(&archive, &dest, compression).unwrap();

            assert_eq!(fs::read(dest.join("proton")).unwrap(), b"#!/bin/sh");
            assert_eq!(fs::read(dest.join("files/lib")).unwrap(), b"x");
            assert!(!dest.join("stale").exists(), "the old folder is replaced, not merged");
            assert!(!dest.with_extension("partial").exists());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// The real listings parse: GE-Proton's latest release has an x86-64
    /// archive and checksum, and each runtime's checksum list names its
    /// archive. Fetches only the small listings, none of the archives.
    /// Run with: `cargo test live_listings -- --ignored --nocapture`
    #[test]
    #[ignore = "needs network access to GitHub and repo.steampowered.com"]
    fn live_listings_parse() {
        let sources = Sources::default();
        let runtime = blocking_runtime().unwrap();

        let json = runtime.block_on(get_text(&sources.ge_proton_latest, true)).unwrap();
        let release = pick_release(&serde_json::from_str(&json).unwrap()).unwrap();
        println!("GE-Proton: {release:?}");
        let sums = runtime.block_on(get_text(&release.checksum_url, false)).unwrap();
        assert_eq!(first_word(&sums).len(), 128, "not a SHA-512: {sums}");

        for spec in &RUNTIMES {
            let base = format!("{}{}", sources.steam_runtimes, spec.path);
            let version = runtime.block_on(get_text(&format!("{base}VERSION.txt"), false)).unwrap();
            let sums = runtime.block_on(get_text(&format!("{base}SHA256SUMS"), false)).unwrap();
            let sum = checksum_for(&sums, spec.archive).unwrap_or_else(|| panic!("{} not listed:
{sums}", spec.archive));
            assert_eq!(sum.len(), 64);
            println!("{}: {} {sum}", spec.name, version.trim());
        }
    }

    // -- the whole download, against a local server ------------------------

    type Routes = std::sync::Arc<std::sync::Mutex<BTreeMap<String, Vec<u8>>>>;

    /// Serves `routes` over HTTP; returns its address and every path asked for.
    fn serve(routes: Routes) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::Write;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    request.push(byte[0]);
                }
                let request = String::from_utf8_lossy(&request);
                let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(path.clone());
                let body = routes.lock().unwrap().get(&path).cloned();
                let (status, body) = match body {
                    Some(body) => ("200 OK", body),
                    None => ("404 Not Found", Vec::new()),
                };
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        (address, asked)
    }

    const RUNTIME_PATH: &str = "/rt/steamrt-images-sniper/snapshots/latest-container-runtime-depot/";

    /// Publish GE-Proton `tag` on the test server. With `corrupt`, its
    /// checksum file does not match the archive.
    fn publish_proton(routes: &Routes, address: &str, tag: &str, corrupt: bool) {
        use sha2::Digest;
        let manifest = b"\"manifest\"\n{\n  \"require_tool_appid\" \"1628350\"\n}\n";
        let archive = gzip(&tarball(tag, &[("proton", b"#!/bin/sh\n"), ("toolmanifest.vdf", manifest)]));
        let mut sum = hex::encode(sha2::Sha512::digest(&archive));
        if corrupt {
            sum = sum.replace(|c: char| c != '0', "0");
        }
        let release = serde_json::json!({
            "tag_name": tag,
            "assets": [
                { "name": format!("{tag}-x86_64.tar.gz"), "browser_download_url": format!("{address}/dl/{tag}-x86_64.tar.gz") },
                { "name": format!("{tag}-x86_64.sha512sum"), "browser_download_url": format!("{address}/dl/{tag}.sum") },
            ]
        });
        let mut routes = routes.lock().unwrap();
        routes.insert("/latest".into(), release.to_string().into_bytes());
        routes.insert(format!("/dl/{tag}-x86_64.tar.gz"), archive);
        routes.insert(format!("/dl/{tag}.sum"), format!("{sum}  {tag}-x86_64.tar.gz\n").into_bytes());
    }

    fn publish_runtime(routes: &Routes, version: &str) {
        use sha2::Digest;
        let archive = xz(&tarball("SteamLinuxRuntime_sniper", &[("_v2-entry-point", b"#!/bin/sh\n")]));
        let sum = hex::encode(sha2::Sha256::digest(&archive));
        let mut routes = routes.lock().unwrap();
        routes.insert(format!("{RUNTIME_PATH}VERSION.txt"), format!("{version}\n").into_bytes());
        routes.insert(format!("{RUNTIME_PATH}SHA256SUMS"), format!("{sum} *SteamLinuxRuntime_sniper.tar.xz\n").into_bytes());
        routes.insert(format!("{RUNTIME_PATH}SteamLinuxRuntime_sniper.tar.xz"), archive);
    }

    #[test]
    fn downloads_ge_proton_and_its_runtime_then_keeps_them_up_to_date() {
        let root = scratch("download");
        let routes = Routes::default();
        let (address, asked) = serve(routes.clone());
        publish_proton(&routes, &address, "GE-Proton-T1", false);
        publish_runtime(&routes, "0.1");
        let managed = Managed::new(
            root.clone(),
            Sources { ge_proton_latest: format!("{address}/latest"), steam_runtimes: format!("{address}/rt/") },
        );
        let said = std::sync::Mutex::new(Vec::new());
        let progress = |label: &str, _: Option<u64>, _: u64| said.lock().unwrap().push(label.to_owned());
        let downloads = |name: &str| asked.lock().unwrap().iter().filter(|p| p.ends_with(name)).count();

        // First time: both are downloaded, checked and unpacked.
        assert_eq!(managed.version(), None);
        let dir = managed.install_proton(false, &progress).unwrap();
        assert_eq!(dir, root.join("runtime/proton/GE-Proton-T1"));
        assert_eq!(managed.version().as_deref(), Some("GE-Proton-T1"));
        let appid = required_runtime(&dir).unwrap();
        let entry = managed.install_runtime(&appid, false, &progress).unwrap();
        assert_eq!(entry, root.join("runtime/steamrt-1628350/_v2-entry-point"));
        assert!(said.lock().unwrap().iter().any(|s| s == "Downloading GE-Proton-T1"), "{said:?}");
        assert!(said.lock().unwrap().iter().any(|s| s == "Unpacking Steam Linux Runtime 3.0 (sniper)"));
        assert!(fs::read_dir(root.join("runtime/downloads")).unwrap().next().is_none(), "archives left behind");

        // Already there: nothing is asked of the network.
        let asked_so_far = asked.lock().unwrap().len();
        managed.install_proton(false, &progress).unwrap();
        managed.install_runtime(&appid, false, &progress).unwrap();
        assert_eq!(asked.lock().unwrap().len(), asked_so_far);

        // An update fetches a newer GE-Proton and drops the old one; a runtime
        // whose version has not changed is not downloaded again.
        publish_proton(&routes, &address, "GE-Proton-T2", false);
        managed.install_proton(true, &progress).unwrap();
        managed.install_runtime(&appid, true, &progress).unwrap();
        assert_eq!(managed.version().as_deref(), Some("GE-Proton-T2"));
        assert!(!root.join("runtime/proton/GE-Proton-T1").exists());
        assert_eq!(downloads("SteamLinuxRuntime_sniper.tar.xz"), 1);

        // A download that fails its checksum installs nothing and keeps the
        // version that works.
        publish_proton(&routes, &address, "GE-Proton-T3", true);
        let error = managed.install_proton(true, &progress).unwrap_err();
        assert!(format!("{error:#}").contains("checksum"), "{error:#}");
        assert_eq!(managed.version().as_deref(), Some("GE-Proton-T2"));
        assert!(!root.join("runtime/proton/GE-Proton-T3").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
