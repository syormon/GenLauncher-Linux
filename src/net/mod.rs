//! Everything that talks to the network: repository manifests, the S3 object
//! store that hosts mod files, and the download engines behind the progress bars.

pub mod download;
pub mod gentool;
pub mod manifests;
pub mod s3;

use once_cell::sync::Lazy;
use std::time::Duration;

use crate::config;

/// Shared client. `reqwest` pools connections internally, so one is enough.
pub static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(format!("GenLauncher/{}", config::VERSION))
        .connect_timeout(Duration::from_secs(15))
        // No overall timeout: mod downloads run for a long time and get their
        // own stall detection instead.
        .build()
        .expect("HTTP client")
});

/// Can we reach the repository index? Decides the launcher's offline mode.
pub async fn check_connection(url: &str) -> bool {
    match CLIENT.get(url).timeout(Duration::from_secs(20)).send().await {
        Ok(resp) => resp.error_for_status().is_ok(),
        Err(_) => false,
    }
}

/// Download `url` into `path` unless it is already there. Used for mod logos;
/// failures are not worth surfacing.
pub async fn download_file_if_missing(
    url: &str,
    dir: &std::path::Path,
    file_name: &str,
) -> anyhow::Result<()> {
    let target = dir.join(file_name);
    if target.exists() {
        return Ok(());
    }
    tokio::fs::create_dir_all(dir).await?;

    let bytes = CLIENT
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    tokio::fs::write(&target, &bytes).await?;
    Ok(())
}
