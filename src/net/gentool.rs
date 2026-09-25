//! The network half of the GenTool check.
//!
//! Uncalled for the same reason as [`crate::game::gentool`].
#![allow(dead_code)]

use anyhow::Result;
use std::time::Duration;

use super::CLIENT;
use crate::game::gentool;

/// Is gentool.net reachable? A failure just means "cannot check".
pub async fn can_connect() -> bool {
    super::check_connection(gentool::GENTOOL_SITE_HTTP).await
}

/// Latest published GenTool version, read off the landing page.
pub async fn latest_version() -> Result<String> {
    let html = CLIENT
        .get(gentool::GENTOOL_SITE)
        .timeout(Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    gentool::parse_latest_version(&html)
        .ok_or_else(|| anyhow::anyhow!("cannot find gentool_ver on {}", gentool::GENTOOL_SITE))
}
