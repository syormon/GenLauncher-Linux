//! Fetching and parsing the YAML manifests published on GitHub.
//! Port of `GitHubYamlReader` and `GitHubMainDataReader`.

use anyhow::{Context, Result};
use std::time::Duration;

use super::CLIENT;
use crate::model::repos::{ReposModsData, VulkanData};
use crate::model::ReposVersion;

const MANIFEST_TIMEOUT: Duration = Duration::from_secs(60);

async fn fetch_yaml<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let text = CLIENT
        .get(url)
        .timeout(MANIFEST_TIMEOUT)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?
        .error_for_status()
        .with_context(|| format!("requesting {url}"))?
        .text()
        .await?;

    serde_yaml_ng::from_str(&text).with_context(|| format!("parsing manifest at {url}"))
}

/// The top-level index that lists every mod, executable and advert.
pub async fn fetch_repos_index(url: &str) -> Result<ReposModsData> {
    fetch_yaml(url).await
}

/// A single modification's manifest.
pub async fn fetch_modification(url: &str) -> Result<ReposVersion> {
    fetch_yaml(url).await
}

/// The Vulkan layer's version manifest.
pub async fn fetch_vulkan_data(url: &str) -> Result<VulkanData> {
    fetch_yaml(url).await
}

/// Fetch many manifests, skipping the ones that fail — a single broken mod
/// manifest must never stop the launcher from starting.
pub async fn fetch_modifications(urls: &[String]) -> Vec<ReposVersion> {
    let mut result = Vec::with_capacity(urls.len());
    for url in urls {
        match fetch_modification(url).await {
            Ok(m) if !m.name.is_empty() => result.push(m),
            Ok(_) => log::warn!("manifest at {url} has no name; skipped"),
            Err(e) => log::warn!("skipping manifest {url}: {e:#}"),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModificationType;

    #[test]
    fn parses_a_real_mod_manifest() {
        let yaml = r#"
ModificationType: Mod
Name: Rise of the Reds
Version: 1.87 Public Build 2.0
SimpleDownloadLink: https://onedrive.live.com/embed?cid=A
UIImageSourceLink: https://i.imgur.com/FNgEp6t.png
DiscordLink: https://discord.gg/REcbv37
ModDBLink: https://www.moddb.com/mods/rise-of-the-reds
NewsLink: https://www.moddb.com/mods/rise-of-the-reds/articles
DependenceName: ''
S3HostLink: gen.insave.ovh:9000
S3BucketName: rotr
S3FolderName: rotr-individual-files
S3HostPublicKey: ''
S3HostSecretKey: ''
"#;
        let m: ReposVersion = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(m.name, "Rise of the Reds");
        assert_eq!(m.modification_type, ModificationType::Mod);
        assert_eq!(m.s3_bucket_name, "rotr");
        assert_eq!(m.mod_db_link, "https://www.moddb.com/mods/rise-of-the-reds");
        assert!(m.dependence_name.is_empty());
    }

    #[test]
    fn parses_the_repository_index() {
        let yaml = r#"
modDatas:
- ModName: Contra
  ModLink: https://example.invalid/ctr.yaml
  ModPatches: []
  ModAddons:
  - https://example.invalid/ctr-hd.yaml
globalAddonsData: []
originalGameAddons: []
originalGamePatches: []
executables:
- ModName: moddedExecutable
  ModLink: https://example.invalid/modded.yaml
  DependencyName: ""
AdvData:
- ModName: Do you like GenLauncher?
  ModLink: https://example.invalid/adv.yaml
  ImagesData:
  - https://example.invalid/1.jpg
LauncherVersion: 1.0.1.0
DownloadLink: https://example.invalid/GenLauncher.zip
VulkanReposData: https://example.invalid/vulkan.yaml
"#;
        let index: ReposModsData = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(index.launcher_version, "1.0.1.0");
        assert_eq!(index.mod_datas.len(), 1);
        assert_eq!(index.mod_datas[0].mod_addons.len(), 1);
        assert_eq!(index.executables[0].mod_name, "moddedExecutable");
        assert_eq!(index.adv_data[0].images_data.len(), 1);
    }
}
