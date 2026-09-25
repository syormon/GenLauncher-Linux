//! Shape of the top-level repository index published on GitHub.
//! Field names match the live YAML exactly (mixed casing is intentional).

use serde::{Deserialize, Serialize};

use super::null_to_empty;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReposModsData {
    #[serde(rename = "LauncherVersion", deserialize_with = "null_to_empty")]
    pub launcher_version: String,
    #[serde(rename = "DownloadLink", deserialize_with = "null_to_empty")]
    pub download_link: String,
    #[serde(rename = "VulkanReposData", deserialize_with = "null_to_empty")]
    pub vulkan_repos_data: String,

    #[serde(rename = "modDatas")]
    pub mod_datas: Vec<ModAddonsAndPatchesRawData>,
    #[serde(rename = "globalAddonsData")]
    pub global_addons_data: Vec<String>,
    #[serde(rename = "originalGameAddons")]
    pub original_game_addons: Vec<String>,
    #[serde(rename = "originalGamePatches")]
    pub original_game_patches: Vec<String>,
    pub executables: Vec<ExecutablesRawData>,
    #[serde(rename = "AdvData")]
    pub adv_data: Vec<AdvertisingData>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModAddonsAndPatchesRawData {
    #[serde(rename = "ModName", deserialize_with = "null_to_empty")]
    pub mod_name: String,
    #[serde(rename = "ModLink", deserialize_with = "null_to_empty")]
    pub mod_link: String,
    #[serde(rename = "ModPatches", deserialize_with = "vec_or_null")]
    pub mod_patches: Vec<String>,
    #[serde(rename = "ModAddons", deserialize_with = "vec_or_null")]
    pub mod_addons: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutablesRawData {
    #[serde(rename = "ModName", deserialize_with = "null_to_empty")]
    pub mod_name: String,
    #[serde(rename = "ModLink", deserialize_with = "null_to_empty")]
    pub mod_link: String,
    #[serde(rename = "DependencyName", deserialize_with = "null_to_empty")]
    pub dependency_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvertisingData {
    #[serde(rename = "ModName", deserialize_with = "null_to_empty")]
    pub mod_name: String,
    #[serde(rename = "ModLink", deserialize_with = "null_to_empty")]
    pub mod_link: String,
    #[serde(rename = "ImagesData", deserialize_with = "vec_or_null")]
    pub images_data: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VulkanData {
    #[serde(rename = "LatestVersion", deserialize_with = "null_to_empty")]
    pub latest_version: String,
    #[serde(rename = "DownloadLink", deserialize_with = "null_to_empty")]
    pub download_link: String,
}

fn vec_or_null<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(Option::<Vec<String>>::deserialize(d)?.unwrap_or_default())
}
