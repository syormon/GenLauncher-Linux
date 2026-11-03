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
/// Reconnects in a row without getting any further, before giving up.
const CONNECTION_ATTEMPTS: u32 = 5;
/// The wait between reconnects doubles from 2 seconds up to this.
const LONGEST_RETRY_WAIT: Duration = Duration::from_secs(30);
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

    let mut retries = Retries::default();
    loop {
        let error = match http_single_file_once(&version, &mut progress, &cancel).await {
            Ok(result) => return result,
            Err(error) => error,
        };
        if let Some(result) = after_failure(error, &mut retries, &progress, &cancel).await {
            return result;
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

    let mut retries = Retries::default();
    loop {
        let attempt =
            s3_multi_file_once(&version, previous_version_folder.as_deref(), &mut progress, &cancel);
        let error = match attempt.await {
            Ok(result) => return result,
            Err(error) => error,
        };
        if let Some(result) = after_failure(error, &mut retries, &progress, &cancel).await {
            return result;
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
            // Flushed either way: the next attempt resumes from the file's size.
            Err(_) => {
                file.flush().await?;
                bail!(TransientError);
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => {
                file.flush().await?;
                return Err(e).context("reading download stream");
            }
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
#[error("connection stalled")]
struct TransientError;

/// Is this a connection failing, which trying again may cure? Besides timeouts
/// and failed connects, that is a connection dropped once it was open: reqwest
/// reports one as a request error while sending, and as a body or decode error
/// partway through a file. Its other errors, such as an HTTP status, are not
/// about the connection.
fn is_transient(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TransientError>().is_some()
        || error.downcast_ref::<reqwest::Error>().is_some_and(|e| {
            e.is_timeout() || e.is_connect() || e.is_request() || e.is_body() || e.is_decode()
        })
}

/// Counts reconnects. Only attempts that got nowhere use them up, so a long
/// install on a flaky connection can lose it any number of times: everything
/// already downloaded is kept, and each attempt continues from there.
#[derive(Debug, Default)]
struct Retries {
    used: u32,
    /// The most bytes any attempt has got to.
    furthest: u64,
}

impl Retries {
    /// After losing the connection with `read` bytes in hand: which attempt
    /// comes next and how long to wait for it, or `None` to give up.
    fn next(&mut self, read: u64) -> Option<(u32, Duration)> {
        if read > self.furthest {
            self.furthest = read;
            self.used = 0;
        }
        if self.used >= CONNECTION_ATTEMPTS {
            return None;
        }
        self.used += 1;
        let wait = Duration::from_secs(2u64.pow(self.used)).min(LONGEST_RETRY_WAIT);
        Some((self.used, wait))
    }
}

/// What to do about a failed attempt: `None` once it is time to try again, or
/// the result the download ends with.
async fn after_failure(
    error: anyhow::Error,
    retries: &mut Retries,
    progress: &Progress,
    cancel: &Cancel,
) -> Option<DownloadResult> {
    if !is_transient(&error) {
        return Some(DownloadResult {
            crashed: true,
            message: format!("{error:#}"),
            ..Default::default()
        });
    }

    log::warn!("download interrupted: {error:#}");
    let Some((attempt, wait)) = retries.next(progress.read) else {
        return Some(DownloadResult {
            timed_out: true,
            message: i18n::tr("ConnectionGaveUp"),
            ..Default::default()
        });
    };

    progress.message(i18n::trf(
        "ConnectionRetrying",
        &[&wait.as_secs().to_string(), &attempt.to_string(), &CONNECTION_ATTEMPTS.to_string()],
    ));
    if !wait_unless_cancelled(wait, cancel).await {
        return Some(cancelled());
    }
    progress.message(i18n::tr("Preparing"));
    None
}

/// Sleep for `wait`, or until the user stops the download. False if they did.
async fn wait_unless_cancelled(wait: Duration, cancel: &Cancel) -> bool {
    const CHECK_EVERY: Duration = Duration::from_millis(200);

    let deadline = Instant::now() + wait;
    while !cancel.is_cancelled() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        tokio::time::sleep(left.min(CHECK_EVERY)).await;
    }
    false
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

    // Written under a temporary name and renamed only once complete, so an
    // interrupted download never leaves a truncated file that later looks
    // installed (a cut-off `modded.exe` would otherwise be run every launch).
    let partial = target_dir.join(format!("{name}.part"));
    let result = async {
        let mut file = tokio::fs::File::create(&partial).await?;
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

        if let Some(total) = total.filter(|total| *total != read) {
            anyhow::bail!("{name}: the download ended after {read} of {total} bytes");
        }
        tokio::fs::rename(&partial, &destination).await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
    }
    result?;

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

    // -- losing the connection ---------------------------------------------

    /// What a test server does with one connection.
    struct Reply {
        bytes: Vec<u8>,
        /// Cut the connection with a reset, rather than closing it normally.
        reset: bool,
    }

    /// Serves one request per connection, as `plan` says from the connection's
    /// number and the request's text. Returns the server's address.
    async fn test_server(
        plan: impl Fn(usize, &str) -> Reply + Send + Sync + 'static,
    ) -> String {
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let plan = Arc::new(plan);

        tokio::spawn(async move {
            let mut connections = 0..;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let number = connections.next().expect("an endless count");
                let plan = plan.clone();
                tokio::spawn(async move {
                    let mut request = [0u8; 4096];
                    let length = socket.read(&mut request).await.unwrap_or(0);
                    let reply = plan(number, &String::from_utf8_lossy(&request[..length]));
                    let _ = socket.write_all(&reply.bytes).await;
                    if reply.reset {
                        // Let the bytes arrive before the reset discards them.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        socket.set_zero_linger().unwrap();
                    }
                });
            }
        });
        address
    }

    fn headers(status: &str, length: usize) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {length}\r\n\
             Content-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
        )
        .into_bytes()
    }

    fn progress() -> Progress {
        Progress::new(ModKey::new(ModificationType::Mod, "test"), std::sync::mpsc::channel().0)
    }

    fn scratch_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gl-download-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// Stream a reply that breaks off after `"partial"`, into `path`.
    async fn stream_cut_off_body(path: &Path, reset: bool) -> anyhow::Error {
        let url = test_server(move |_, _| Reply {
            bytes: [headers("200 OK", 1000), b"partial".to_vec()].concat(),
            reset,
        })
        .await;
        let response = reqwest::Client::new().get(url).send().await.unwrap();
        let file = tokio::fs::File::create(path).await.unwrap();

        stream_to_file(response, file, &mut progress(), &Cancel::new(), None)
            .await
            .expect_err("the body was cut off")
    }

    #[tokio::test]
    async fn a_reset_before_any_reply_is_worth_retrying() {
        let url = test_server(|_, _| Reply { bytes: Vec::new(), reset: true }).await;

        let error = reqwest::Client::new().get(url).send().await.unwrap_err();

        assert!(is_transient(&error.into()));
    }

    #[tokio::test]
    async fn a_connection_lost_partway_through_a_file_is_worth_retrying() {
        for (name, reset) in [("reset", true), ("closed", false)] {
            let path = scratch_file(name);

            let error = stream_cut_off_body(&path, reset).await;

            assert!(is_transient(&error), "{name}: {error:#}");
            // The next attempt resumes from the size of what was written.
            assert_eq!(std::fs::read(&path).unwrap(), b"partial", "{name}");
        }
    }

    #[tokio::test]
    async fn an_http_error_is_not_a_connection_problem() {
        let url =
            test_server(|_, _| Reply { bytes: headers("404 Not Found", 0), reset: false }).await;

        let response = reqwest::Client::new().get(url).send().await.unwrap();
        let error = response.error_for_status().unwrap_err();

        assert!(!is_transient(&error.into()));
        assert!(!is_transient(&anyhow::anyhow!("the disk is full")));
    }

    #[test]
    fn retries_wait_longer_each_time_then_give_up() {
        let mut retries = Retries::default();
        let waits: Vec<u64> = (0..CONNECTION_ATTEMPTS)
            .map(|_| retries.next(0).expect("attempts left").1.as_secs())
            .collect();

        assert_eq!(waits, [2, 4, 8, 16, 30]);
        assert_eq!(retries.next(0), None);
    }

    #[test]
    fn any_progress_earns_a_fresh_set_of_retries() {
        let mut retries = Retries::default();
        for read in [100, 100, 100] {
            retries.next(read).unwrap();
        }
        assert_eq!(retries.next(100).unwrap().0, 4, "no further on: the count goes on");

        // Further than ever: back to the first attempt and the shortest wait.
        assert_eq!(retries.next(101), Some((1, Duration::from_secs(2))));
        // Falling short of that later is not progress.
        assert_eq!(retries.next(50).unwrap().0, 2);
    }

    #[tokio::test]
    async fn only_connection_failures_are_retried() {
        let result = after_failure(
            anyhow::anyhow!("the disk is full"),
            &mut Retries::default(),
            &progress(),
            &Cancel::new(),
        )
        .await
        .expect("no retry");

        assert!(result.crashed);
        assert_eq!(result.message, "the disk is full");
    }

    #[tokio::test]
    async fn running_out_of_retries_says_what_to_do() {
        let mut spent = Retries { used: CONNECTION_ATTEMPTS, furthest: 0 };

        let result =
            after_failure(TransientError.into(), &mut spent, &progress(), &Cancel::new())
                .await
                .expect("no retry");

        assert!(result.timed_out && !result.crashed);
        assert_eq!(result.message, i18n::tr("ConnectionGaveUp"));
    }

    #[tokio::test]
    async fn stop_works_during_the_wait_before_a_retry() {
        // The longest wait there is, so that sitting it out would fail.
        let mut retries = Retries { used: CONNECTION_ATTEMPTS - 1, furthest: 0 };
        let cancel = Cancel::new();
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            stopper.cancel();
        });

        let started = Instant::now();
        let result = after_failure(TransientError.into(), &mut retries, &progress(), &cancel)
            .await
            .expect("no retry");

        assert!(result.canceled);
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn the_wait_before_a_retry_runs_its_course() {
        let started = Instant::now();
        assert!(wait_unless_cancelled(Duration::from_millis(250), &Cancel::new()).await);
        assert!(started.elapsed() >= Duration::from_millis(250));
    }

    /// The whole of a download through a connection that keeps failing: a
    /// reset before any reply, then one partway through the file. It should
    /// resume from where it broke and end up with the file intact.
    ///
    /// Ignored because it claims the process-wide game folder, which other
    /// tests share. Run it alone:
    ///
    /// ```text
    /// cargo test survives_losing_the_connection -- --ignored
    /// ```
    #[tokio::test]
    #[ignore = "claims the process-wide game folder; run it on its own"]
    async fn survives_losing_the_connection() {
        use crate::model::ReposVersion;

        let game = std::env::temp_dir().join(format!("gl-flaky-game-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&game);
        std::fs::create_dir_all(&game).unwrap();
        config::set_game_dir(game.clone());
        assert_eq!(config::game_dir(), game, "another test claimed the game folder first");

        let content: Vec<u8> = (0..100_000u32).map(|n| (n % 251) as u8).collect();
        let served = content.clone();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = requests.clone();

        let url = test_server(move |connection, request| {
            let range = request.lines().find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower.strip_prefix("range: bytes=")?.strip_suffix('-')?.parse::<usize>().ok()
            });
            log.lock().unwrap().push(range);

            if let Some(from) = range {
                let rest = &served[from..];
                return Reply {
                    bytes: [headers("206 Partial Content", rest.len()), rest.to_vec()].concat(),
                    reset: false,
                };
            }
            match connection {
                0 => Reply { bytes: Vec::new(), reset: true },
                1 => Reply {
                    bytes: [headers("200 OK", served.len()), served[..40_000].to_vec()].concat(),
                    reset: true,
                },
                _ => Reply {
                    bytes: [headers("200 OK", served.len()), served.clone()].concat(),
                    reset: false,
                },
            }
        })
        .await;

        let version = ModVersion {
            info: ReposVersion {
                name: "Flaky Mod".into(),
                version: "1.0".into(),
                modification_type: ModificationType::Mod,
                simple_download_link: format!("{url}/mod.big"),
                ..Default::default()
            },
            ..Default::default()
        };

        let (tx, rx) = std::sync::mpsc::channel();
        let result = http_single_file(version.clone(), tx, Cancel::new()).await;

        assert!(!result.crashed && !result.timed_out, "{}", result.message);
        assert_eq!(std::fs::read(version.folder_path().join("mod.gib")).unwrap(), content);

        let said: Vec<String> = rx
            .try_iter()
            .filter_map(|event| match event {
                DownloadEvent::Message { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        let retry = |wait: &str, attempt: &str| {
            i18n::trf("ConnectionRetrying", &[wait, attempt, &CONNECTION_ATTEMPTS.to_string()])
        };
        assert!(said.contains(&retry("2", "1")), "{said:?}");
        assert_eq!(said.iter().filter(|text| **text == retry("2", "1")).count(), 2, "{said:?}");

        // The last request asked for just the rest of the file.
        assert_eq!(requests.lock().unwrap().last(), Some(&Some(40_000)));
        let _ = std::fs::remove_dir_all(&game);
    }
}

