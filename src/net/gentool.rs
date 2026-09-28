//! The network half of the GenTool check.

use anyhow::Result;
use std::time::Duration;

use super::CLIENT;
use crate::game::gentool;

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
