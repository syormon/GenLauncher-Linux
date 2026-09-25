//! The two download engines behind every progress bar in the launcher.
//!
//! * [`http_single_file`] fetches one archive from a share link and unpacks it —
//!   the port of `HttpSingleFileUpdater`.
//! * [`s3_multi_file`] syncs a modification file-by-file out of its object
//!   store, reusing unchanged files from the previous version — the port of
//!   `S3Updater`.
//!
//! Both write into a `*.GLTC` staging folder and only swap it into place once
//! the whole transfer has succeeded, so an interrupted update never leaves a
//! half-installed modification behind.

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

use super::CLIENT;
use crate::config;
use crate::game::launcher::RemoteFileInfo;
use crate::i18n;
use crate::model::{ModVersion, ModificationType};
use crate::util::{archive, fs as gfs, links, md5_file};

/// Give up on a transfer that has not produced a byte for this long.
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECTION_ATTEMPTS: u32 = 5;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(120);

/// Identifies the list row a download belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModKey {
    pub kind: ModificationType,
    pub name: String,
}

impl ModKey {
    pub fn of(version: &ModVersion) -> Self {
        Self { kind: version.kind(), name: version.name().to_ascii_lowercase() }
    }

    pub fn new(kind: ModificationType, name: &str) -> Self {
        Self { kind, name: name.to_ascii_lowercase() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DownloadResult {
    pub crashed: bool,
    pub canceled: bool,
    pub timed_out: bool,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Progress {
        key: ModKey,
        total: Option<u64>,
        read: u64,
        percent: Option<f64>,
        file: Option<String>,
    },
    Message {
        key: ModKey,
        text: String,
    },
    Done {
        key: ModKey,
        result: DownloadResult,
    },
}

/// Lets the UI stop a running transfer.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Channel the engines report through. Cloned into each download task.
pub type Reporter = std::sync::mpsc::Sender<DownloadEvent>;

struct Progress {
    key: ModKey,
    tx: Reporter,
    total: Option<u64>,
    read: u64,
    last_sent: Instant,
}

impl Progress {
    fn new(key: ModKey, tx: Reporter) -> Self {
        Self { key, tx, total: None, read: 0, last_sent: Instant::now() - PROGRESS_INTERVAL }
    }

    fn message(&self, text: impl Into<String>) {
        let _ = self.tx.send(DownloadEvent::Message {
            key: self.key.clone(),
            text: text.into(),
        });
    }

    fn emit(&mut self, file: Option<&str>, force: bool) {
        if !force && self.last_sent.elapsed() < PROGRESS_INTERVAL {
            return;
        }
        self.last_sent = Instant::now();
        let percent = self
            .total
            .filter(|t| *t > 0)
            .map(|t| (self.read as f64 / t as f64 * 100.0).min(100.0));
        let _ = self.tx.send(DownloadEvent::Progress {
            key: self.key.clone(),
            total: self.total,
            read: self.read,
            percent,
            file: file.map(str::to_owned),
        });
    }
}

/// Staging folder for `version`, created if needed.
fn staging_folder(version: &ModVersion) -> Result<PathBuf> {
    let path = PathBuf::from(format!(
        "{}{}",
        version.folder_path().display(),
        config::VERSION_FOLDER_COPY_SUFFIX
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Swap the finished staging folder into the version's real folder.
fn commit_staging(staging: &Path, version: &ModVersion) -> Result<()> {
    let final_path = version.folder_path();
    if final_path.exists() {
        std::fs::remove_dir_all(&final_path)?;
    }
    gfs::move_dir(staging, &final_path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Single-file HTTP downloads
// ---------------------------------------------------------------------------

/// Download the modification's archive from its share link and unpack it.
pub async fn http_single_file(
    version: ModVersion,
    tx: Reporter,
    cancel: Cancel,
) -> DownloadResult {
    let key = ModKey::of(&version);
    let mut progress = Progress::new(key.clone(), tx.clone());
    progress.message(i18n::tr("Preparing"));

    let mut attempt = 0u32;
    loop {
        match http_single_file_once(&version, &mut progress, &cancel).await {
            Ok(result) => return result,
            Err(e) if is_transient(&e) && attempt < CONNECTION_ATTEMPTS => {
                attempt += 1;
                progress.message(format!(
                    "Connection timed out. Trying to reastablish connection... attempt: {attempt}"
                ));
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) if is_transient(&e) => {
                return DownloadResult {
                    timed_out: true,
                    message: "Connection timed out. Unable to establish a new connection".into(),
                    ..Default::default()
                }
            }
            Err(e) => {
                return DownloadResult {
                    crashed: true,
                    message: format!("{e:#}"),
                    ..Default::default()
                }
            }
        }
    }
}

async fn http_single_file_once(
    version: &ModVersion,
    progress: &mut Progress,
    cancel: &Cancel,
) -> Result<DownloadResult> {
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }

    let url = links::parse_download_link(&version.info.simple_download_link);
    if url.is_empty() {
        bail!("this modification has no download link");
    }

    let head = CLIENT.get(&url).send().await?.error_for_status()?;

    // Guard against a share link that resolves to an HTML landing page.
    if let Some(content_type) = head.headers().get(reqwest::header::CONTENT_TYPE) {
        let ct = content_type.to_str().unwrap_or("");
        let has_disposition = head.headers().contains_key(reqwest::header::CONTENT_DISPOSITION);
        if !has_disposition
            && !ct.starts_with("application/zip")
            && !ct.starts_with("application/octet-stream")
            && !ct.starts_with("application/x-")
        {
            bail!("Download link is incorrect, please contact modification creator and try again later.");
        }
    }

    let file_name = file_name_from_response(&head, &url);
    let total = head.content_length();

    let staging = staging_folder(version)?;
    let destination = staging.join(&file_name);

    // Resume a partial file from a previous attempt.
    let already = std::fs::metadata(&destination).map(|m| m.len()).unwrap_or(0);
    let resume = already > 0 && total.map(|t| already < t).unwrap_or(false);

    let response = if resume {
        CLIENT
            .get(&url)
            .header(reqwest::header::RANGE, format!("bytes={already}-"))
            .send()
            .await?
            .error_for_status()?
    } else {
        head
    };

    progress.total = total;
    progress.read = if resume { already } else { 0 };

    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resume)
        .truncate(!resume)
        .open(&destination)
        .await?;

    if !stream_to_file(response, file, progress, cancel, None).await? {
        return Ok(cancelled());
    }

    progress.emit(None, true);
    progress.message(i18n::tr("UnpackingPreparing"));

    // Unpacking is CPU- and disk-bound; keep it off the async runtime.
    let destination_for_task = destination.clone();
    let staging_for_task = staging.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if archive::is_supported_archive(&destination_for_task) {
            archive::extract(&destination_for_task, &staging_for_task, true)?;
            std::fs::remove_file(&destination_for_task)?;
        } else if gfs::extension_of(&destination_for_task) == "big" {
            // A bare .big is stored under .gib like every other mod archive.
            let gib = gfs::change_extension(&destination_for_task, "gib");
            let _ = std::fs::remove_file(&gib);
            gfs::move_file(&destination_for_task, &gib)?;
        }
        Ok(())
    })
    .await??;

    let version_for_task = version.clone();
    tokio::task::spawn_blocking(move || commit_staging(&staging, &version_for_task)).await??;

    Ok(DownloadResult::default())
}

fn file_name_from_response(response: &reqwest::Response, url: &str) -> String {
    if let Some(value) = response.headers().get(reqwest::header::CONTENT_DISPOSITION) {
        if let Ok(text) = value.to_str() {
            if let Some(name) = parse_content_disposition(text) {
                return name;
            }
        }
    }

    // Fall back to the last path segment of the final URL.
    let from_url = response
        .url()
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned())
        .or_else(|| url.rsplit('/').next().map(str::to_owned))
        .unwrap_or_default();

    let cleaned = gfs::sanitize_file_name(from_url.split('?').next().unwrap_or(""));
    if cleaned.is_empty() {
        "GenLauncherDownloadingFile".to_owned()
    } else {
        cleaned
    }
}

fn parse_content_disposition(header: &str) -> Option<String> {
    // `filename*=UTF-8''name.zip` wins over plain `filename="name.zip"`.
    let extended = header.split(';').map(str::trim).find_map(|part| {
        let value = part.strip_prefix("filename*=")?;
        let encoded = value.rsplit('\'').next()?;
        Some(crate::util::percent_decode(encoded))
    });

    let plain = header.split(';').map(str::trim).find_map(|part| {
        let value = part.strip_prefix("filename=")?;
        Some(value.trim_matches('"').replace('\\', ""))
    });

    let name = extended.or(plain)?;
    let cleaned = gfs::sanitize_file_name(&name);
    (!cleaned.is_empty()).then_some(cleaned)
}


// ---------------------------------------------------------------------------
// S3 file-by-file sync
// ---------------------------------------------------------------------------

/// Sync a modification out of its S3 bucket, file by file.
pub async fn s3_multi_file(
    version: ModVersion,
    previous_version_folder: Option<PathBuf>,
    tx: Reporter,
    cancel: Cancel,
) -> DownloadResult {
    let key = ModKey::of(&version);
    let mut progress = Progress::new(key.clone(), tx.clone());
    progress.message(i18n::tr("Preparing"));

    let mut attempt = 0u32;
    loop {
        match s3_multi_file_once(&version, previous_version_folder.as_deref(), &mut progress, &cancel)
            .await
        {
            Ok(result) => return result,
            Err(e) if is_transient(&e) && attempt < CONNECTION_ATTEMPTS => {
                attempt += 1;
                progress.message(format!(
                    "Connection timed out. Trying to reastablish connection... attempt: {attempt}"
                ));
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) if is_transient(&e) => {
                return DownloadResult {
                    timed_out: true,
                    message: "Connection timed out. Unable to establish a new connection".into(),
                    ..Default::default()
                }
            }
            Err(e) => {
                return DownloadResult {
                    crashed: true,
                    message: format!("{e:#}"),
                    ..Default::default()
                }
            }
        }
    }
}

async fn s3_multi_file_once(
    version: &ModVersion,
    previous_folder: Option<&Path>,
    progress: &mut Progress,
    cancel: &Cancel,
) -> Result<DownloadResult> {
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }

    let remote = super::s3::list_mod_files(&version.info).await?;
    if remote.is_empty() {
        bail!("the modification's storage is empty");
    }

    let staging = staging_folder(version)?;

    // Anything unchanged since the last installed version is copied locally
    // rather than downloaded again.
    if let Some(previous) = previous_folder {
        let previous = previous.to_path_buf();
        let staging_for_task = staging.clone();
        let remote_for_task = remote.clone();
        progress.message(i18n::tr("Preparing"));
        tokio::task::spawn_blocking(move || {
            copy_unchanged_files(&previous, &staging_for_task, &remote_for_task)
        })
        .await??;
    }

    progress.total = Some(remote.iter().map(|f| f.size).sum());
    progress.read = 0;

    for info in &remote {
        loop {
            if cancel.is_cancelled() {
                return Ok(cancelled());
            }

            let destination = staging.join(&info.file_name);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let downloaded_bytes =
                download_one_s3_file(version, info, &destination, progress, cancel).await?;
            if cancel.is_cancelled() {
                return Ok(cancelled());
            }

            let final_path = finalize_big_extension(&destination)?;

            if verify_file(&final_path, info).await? {
                break;
            }

            // Bad checksum: drop it and retry this file from scratch.
            let _ = std::fs::remove_file(&final_path);
            progress.message("Hash sum mismatch detected, restart file download");
            progress.read = progress.read.saturating_sub(downloaded_bytes);
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    progress.emit(None, true);

    let version_for_task = version.clone();
    tokio::task::spawn_blocking(move || commit_staging(&staging, &version_for_task)).await??;

    Ok(DownloadResult::default())
}

/// Fetch one object, resuming a partial file. Returns bytes accounted for.
async fn download_one_s3_file(
    version: &ModVersion,
    info: &RemoteFileInfo,
    destination: &Path,
    progress: &mut Progress,
    cancel: &Cancel,
) -> Result<u64> {
    // The file may already be here in full, or as a renamed .gib.
    let existing = [destination.to_path_buf(), gfs::change_extension(destination, "gib")]
        .into_iter()
        .find_map(|p| std::fs::metadata(&p).ok().map(|m| (p, m.len())));

    let already = existing.as_ref().map(|(_, len)| *len).unwrap_or(0);

    if already >= info.size && info.size > 0 {
        progress.read += info.size;
        progress.emit(Some(&info.file_name), false);
        return Ok(info.size);
    }

    progress.read += already;

    let url = super::s3::file_url(&version.info, &info.file_name);
    let mut request = CLIENT.get(&url);
    if already > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={already}-"));
    }
    let response = request.send().await?.error_for_status()?;

    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(already > 0)
        .truncate(already == 0)
        .open(destination)
        .await?;

    let before = progress.read;
    stream_to_file(response, file, progress, cancel, Some(&info.file_name)).await?;
    Ok(progress.read.saturating_sub(before) + already)
}

/// Mod archives are stored as `.gib` so the game cannot pick them up directly.
fn finalize_big_extension(path: &Path) -> Result<PathBuf> {
    if gfs::extension_of(path) != "big" {
        let gib = gfs::change_extension(path, "gib");
        return Ok(if !path.exists() && gib.exists() { gib } else { path.to_path_buf() });
    }
    let gib = gfs::change_extension(path, "gib");
    if path.exists() {
        let _ = std::fs::remove_file(&gib);
        gfs::move_file(path, &gib)?;
    }
    Ok(gib)
}

/// Checksum-verify a freshly downloaded file, skipping types the repo does not hash.
async fn verify_file(path: &Path, info: &RemoteFileInfo) -> Result<bool> {
    const HASHED: &[&str] =
        &["w3d", "big", "bik", "gib", "dds", "tga", "ini", "scb", "wnd", "csf", "str"];

    if !HASHED.contains(&gfs::extension_of(path).as_str()) {
        return Ok(true);
    }
    if !path.exists() {
        return Ok(false);
    }

    let path = path.to_path_buf();
    let expected = info.hash.clone();
    Ok(tokio::task::spawn_blocking(move || {
        md5_file(&path).map(|h| h.eq_ignore_ascii_case(&expected)).unwrap_or(false)
    })
    .await?)
}

/// Copy files from the previous version whose hash already matches the listing.
fn copy_unchanged_files(
    previous: &Path,
    staging: &Path,
    remote: &[RemoteFileInfo],
) -> Result<()> {
    if !previous.exists() {
        return Ok(());
    }
    copy_unchanged_in(previous, staging, remote, "")
}

fn copy_unchanged_in(
    source_dir: &Path,
    target_dir: &Path,
    remote: &[RemoteFileInfo],
    relative: &str,
) -> Result<()> {
    std::fs::create_dir_all(target_dir)?;

    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let source = entry.path();
        let name = gfs::file_name_of(&source);

        if entry.file_type()?.is_dir() {
            let nested =
                if relative.is_empty() { name.clone() } else { format!("{relative}/{name}") };
            copy_unchanged_in(&source, &target_dir.join(&name), remote, &nested)?;
            continue;
        }

        let target = target_dir.join(&name);
        if target.exists() {
            continue;
        }

        let Ok(hash) = md5_file(&source) else { continue };
        let relative_name =
            if relative.is_empty() { name.clone() } else { format!("{relative}/{name}") };

        if remote.iter().any(|info| remote_matches(info, &relative_name, &hash)) {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// A local file matches a remote entry when the hash agrees and the name does
/// too, ignoring the extension — a local `.gib` stands in for a remote `.big`.
fn remote_matches(info: &RemoteFileInfo, relative_name: &str, hash: &str) -> bool {
    if !info.hash.eq_ignore_ascii_case(hash) {
        return false;
    }
    if info.file_name.eq_ignore_ascii_case(relative_name) {
        return true;
    }
    strip_extension(&info.file_name).eq_ignore_ascii_case(&strip_extension(relative_name))
}

fn strip_extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !ext.contains('/') => stem.to_owned(),
        _ => name.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Shared streaming
// ---------------------------------------------------------------------------

/// Pump a response body into `file`. Returns `false` when the user cancelled.
async fn stream_to_file(
    response: reqwest::Response,
    mut file: tokio::fs::File,
    progress: &mut Progress,
    cancel: &Cancel,
    label: Option<&str>,
) -> Result<bool> {
    let mut stream = response.bytes_stream();

    loop {
        if cancel.is_cancelled() {
            file.flush().await?;
            return Ok(false);
        }

        let next = tokio::time::timeout(STALL_TIMEOUT, stream.next()).await;
        let chunk = match next {
            Err(_) => bail!(TransientError),
            Ok(None) => break,
            Ok(Some(Err(e))) if e.is_timeout() || e.is_connect() => bail!(TransientError),
            Ok(Some(Err(e))) => return Err(e).context("reading download stream"),
            Ok(Some(Ok(chunk))) => chunk,
        };

        file.write_all(&chunk).await?;
        progress.read += chunk.len() as u64;
        progress.emit(label, false);
    }

    file.flush().await?;
    progress.emit(label, true);
    Ok(true)
}

/// Marker for "the connection stalled, retrying is worthwhile".
#[derive(Debug, thiserror::Error)]
#[error("connection timed out")]
struct TransientError;

fn is_transient(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TransientError>().is_some()
        || error
            .downcast_ref::<reqwest::Error>()
            .map(|e| e.is_timeout() || e.is_connect())
            .unwrap_or(false)
}

fn cancelled() -> DownloadResult {
    DownloadResult {
        canceled: true,
        message: i18n::tr("Canceled"),
        ..Default::default()
    }
}

/// Download and unpack a standalone file: the launcher's own update, GenTool,
/// World Builder, the modded executable, the Vulkan layer.
pub async fn simple_file(
    url: &str,
    target_dir: &Path,
    extract: bool,
    mut on_progress: impl FnMut(Option<u64>, u64),
) -> Result<PathBuf> {
    std::fs::create_dir_all(target_dir)?;

    let response = CLIENT.get(url).send().await?.error_for_status()?;
    let total = response.content_length();
    let name = file_name_from_response(&response, url);
    let destination = target_dir.join(&name);

    let mut file = tokio::fs::File::create(&destination).await?;
    let mut stream = response.bytes_stream();
    let mut read = 0u64;

    while let Some(chunk) = tokio::time::timeout(STALL_TIMEOUT, stream.next())
        .await
        .map_err(|_| TransientError)?
    {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        read += chunk.len() as u64;
        on_progress(total, read);
    }
    file.flush().await?;
    drop(file);

    if extract && archive::is_supported_archive(&destination) {
        let destination_for_task = destination.clone();
        let target = target_dir.to_path_buf();
        let extracted = tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>> {
            let files = archive::extract(&destination_for_task, &target, false)?;
            std::fs::remove_file(&destination_for_task)?;
            Ok(files)
        })
        .await??;

        if let Some(last) = extracted.into_iter().next_back() {
            return Ok(last);
        }
    }

    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_filename_out_of_content_disposition() {
        assert_eq!(
            parse_content_disposition("attachment; filename=\"ROTR.7z\"").as_deref(),
            Some("ROTR.7z")
        );
        assert_eq!(
            parse_content_disposition("attachment; filename*=UTF-8''Contra%20009.zip").as_deref(),
            Some("Contra 009.zip")
        );
        assert_eq!(parse_content_disposition("inline"), None);
    }

    #[test]
    fn a_local_gib_satisfies_a_remote_big() {
        let info = RemoteFileInfo {
            file_name: "Data/ROTR.big".into(),
            hash: "ABC".into(),
            size: 10,
        };
        assert!(remote_matches(&info, "Data/ROTR.gib", "abc"));
        assert!(remote_matches(&info, "Data/ROTR.big", "ABC"));
        assert!(!remote_matches(&info, "Data/OTHER.gib", "ABC"), "name must still match");
        assert!(!remote_matches(&info, "Data/ROTR.gib", "DEF"), "hash must still match");
    }

    /// Downloads one real file end to end and checks what lands on disk.
    /// Run with: `cargo test -- --ignored live_download`
    #[test]
    #[ignore = "requires network access to gen.insave.ovh"]
    fn live_download_writes_the_literal_file_name() {
        use crate::model::ReposVersion;

        let info = ReposVersion {
            name: "Operation Firestorm".into(),
            version: "livetest".into(),
            s3_host_link: "gen.insave.ovh:9000".into(),
            s3_bucket_name: "operationfirestorm".into(),
            s3_folder_name: "ofs-individual-files".into(),
            ..Default::default()
        };
        let version = ModVersion { info, ..Default::default() };

        let runtime =
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

        let files = runtime
            .block_on(crate::net::s3::list_mod_files(&version.info))
            .expect("listing");

        // The smallest object keeps the test quick; pick one whose name has the
        // leading '!' that used to come back percent-encoded.
        let target = files
            .iter()
            .filter(|f| f.file_name.starts_with('!'))
            .min_by_key(|f| f.size)
            .expect("a '!'-prefixed file");

        let staging = std::env::temp_dir().join("gl-live-download");
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).unwrap();

        let destination = staging.join(&target.file_name);
        let mut progress = Progress::new(ModKey::of(&version), std::sync::mpsc::channel().0);
        let cancel = Cancel::new();

        runtime
            .block_on(download_one_s3_file(
                &version,
                target,
                &destination,
                &mut progress,
                &cancel,
            ))
            .expect("download");

        let written = finalize_big_extension(&destination).expect("rename");
        let name = gfs::file_name_of(&written);

        assert!(!name.contains('%'), "percent escape leaked into {name}");
        assert_eq!(
            name,
            gfs::change_extension(std::path::Path::new(&target.file_name), "gib")
                .to_string_lossy(),
            "a .big must be stored as .gib under its real name"
        );
        assert_eq!(
            std::fs::metadata(&written).unwrap().len(),
            target.size,
            "downloaded size must match the listing"
        );

        let _ = std::fs::remove_dir_all(&staging);
    }
}

