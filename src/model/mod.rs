//! Domain model: the modification hierarchy and the persisted launcher config.
//!
//! Field names are deliberately kept identical to the C# originals so existing
//! `GenLauncherCfg.yaml` files and the published repository manifests keep
//! deserializing unchanged.

pub mod colors;
pub mod repos;

use serde::{Deserialize, Deserializer, Serialize};
use std::cmp::Ordering;

use crate::config;
use colors::ColorsInfoString;

/// Read any YAML scalar as a string, the way YamlDotNet coerced them.
///
/// This matters: mod versions are routinely written unquoted (`Version: 1.86`),
/// which YAML resolves to a number. The C# deserializer converted that to
/// `"1.86"` and the launcher then used it as a folder name, so a strict string
/// deserializer here would reject real configs and repository manifests.
/// `null` becomes the empty string, as it did before.
pub fn null_to_empty<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(scalar_to_string(&serde_yaml_ng::Value::deserialize(d)?))
}

fn null_to_empty_vec<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let value = serde_yaml_ng::Value::deserialize(d)?;
    Ok(match value {
        serde_yaml_ng::Value::Sequence(items) => {
            items.iter().map(scalar_to_string).collect()
        }
        serde_yaml_ng::Value::Null => Vec::new(),
        // A lone scalar where a list was expected is treated as a list of one.
        other => vec![scalar_to_string(&other)],
    })
}

/// `2.0` stays `"2.0"` and `1.86` stays `"1.86"`, so folder names still match.
fn scalar_to_string(value: &serde_yaml_ng::Value) -> String {
    match value {
        serde_yaml_ng::Value::String(s) => s.clone(),
        serde_yaml_ng::Value::Number(n) => n.to_string(),
        serde_yaml_ng::Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ModificationType {
    #[default]
    Mod,
    Addon,
    Patch,
    Advertising,
    Executable,
}

/// One published version of a mod/addon/patch/executable, as described by a
/// repository manifest. Mirrors `ModificationReposVersion`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ReposVersion {
    pub modification_type: ModificationType,
    #[serde(deserialize_with = "null_to_empty")]
    pub name: String,
    #[serde(deserialize_with = "null_to_empty")]
    pub version: String,
    #[serde(deserialize_with = "null_to_empty")]
    pub simple_download_link: String,
    #[serde(rename = "UIImageSourceLink", deserialize_with = "null_to_empty")]
    pub ui_image_source_link: String,
    #[serde(deserialize_with = "null_to_empty")]
    pub discord_link: String,
    #[serde(rename = "ModDBLink", deserialize_with = "null_to_empty")]
    pub mod_db_link: String,
    #[serde(deserialize_with = "null_to_empty")]
    pub news_link: String,
    #[serde(deserialize_with = "null_to_empty")]
    pub dependence_name: String,

    #[serde(rename = "S3HostLink", deserialize_with = "null_to_empty")]
    pub s3_host_link: String,
    #[serde(rename = "S3BucketName", deserialize_with = "null_to_empty")]
    pub s3_bucket_name: String,
    #[serde(rename = "S3FolderName", deserialize_with = "null_to_empty")]
    pub s3_folder_name: String,
    #[serde(rename = "S3HostPublicKey", deserialize_with = "null_to_empty")]
    pub s3_host_public_key: String,
    #[serde(rename = "S3HostSecretKey", deserialize_with = "null_to_empty")]
    pub s3_host_secret_key: String,

    #[serde(deserialize_with = "null_to_empty")]
    pub network_info: String,
    pub deprecated: bool,
    #[serde(deserialize_with = "null_to_empty")]
    pub support_link: String,

    pub colors_information: Option<ColorsInfoString>,

    #[serde(deserialize_with = "null_to_empty_vec")]
    pub exception_names: Vec<String>,
    #[serde(deserialize_with = "null_to_empty_vec")]
    pub additional_file_names: Vec<String>,
    #[serde(deserialize_with = "null_to_empty")]
    pub executable_file_name: String,
    pub replaces_original_game_file: bool,
    #[serde(rename = "ReplacesOriginalWBFile")]
    pub replaces_original_wb_file: bool,
}

/// A repos version plus the local install/selection state. Mirrors `ModificationVersion`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModVersion {
    #[serde(flatten)]
    pub info: ReposVersion,
    #[serde(rename = "IsSelected")]
    pub is_selected: bool,
    #[serde(rename = "Installed")]
    pub installed: bool,
}

impl From<ReposVersion> for ModVersion {
    fn from(info: ReposVersion) -> Self {
        Self { info, is_selected: false, installed: false }
    }
}

impl ModVersion {
    pub fn name(&self) -> &str {
        &self.info.name
    }
    pub fn version(&self) -> &str {
        &self.info.version
    }
    pub fn kind(&self) -> ModificationType {
        self.info.modification_type
    }

    /// Name + version, case-insensitively — the C# `ModificationVersion.Equals`.
    pub fn same_identity(&self, other: &ModVersion) -> bool {
        self.info.name.eq_ignore_ascii_case(&other.info.name)
            && self.info.version.eq_ignore_ascii_case(&other.info.version)
    }

    /// Folder this version's files live in, relative to the game directory.
    pub fn folder_name(&self) -> String {
        let m = config::MODS_FOLDER;
        match self.kind() {
            ModificationType::Mod | ModificationType::Advertising => {
                format!("{m}/{}/{}", self.info.name, self.info.version)
            }
            ModificationType::Addon => format!(
                "{m}/{}/{}/{}/{}",
                self.info.dependence_name,
                config::ADDONS_FOLDER_NAME,
                self.info.name,
                self.info.version
            ),
            ModificationType::Patch => format!(
                "{m}/{}/{}/{}/{}",
                self.info.dependence_name,
                config::PATCHES_FOLDER_NAME,
                self.info.name,
                self.info.version
            ),
            ModificationType::Executable => format!(
                "{m}/{}/{}/{}/{}",
                self.info.dependence_name,
                config::EXECUTABLES_FOLDER_NAME,
                self.info.name,
                self.info.version
            ),
        }
    }

    pub fn folder_path(&self) -> std::path::PathBuf {
        config::game_path(self.folder_name())
    }

    /// Merge another record of the same version into this one. Non-empty fields
    /// win only where this one is still empty — faithful to `UnionModifications`.
    pub fn union(&mut self, other: &ModVersion) {
        self.is_selected |= other.is_selected;
        self.installed |= other.installed;

        let a = &mut self.info;
        let b = &other.info;

        fill(&mut a.simple_download_link, &b.simple_download_link);
        if b.modification_type != ModificationType::Mod
            && a.modification_type == ModificationType::Mod
        {
            a.modification_type = b.modification_type;
        }
        fill(&mut a.ui_image_source_link, &b.ui_image_source_link);
        fill(&mut a.dependence_name, &b.dependence_name);
        fill(&mut a.news_link, &b.news_link);
        fill(&mut a.mod_db_link, &b.mod_db_link);
        fill(&mut a.discord_link, &b.discord_link);
        fill(&mut a.network_info, &b.network_info);
        fill(&mut a.support_link, &b.support_link);
        fill(&mut a.s3_bucket_name, &b.s3_bucket_name);
        fill(&mut a.s3_folder_name, &b.s3_folder_name);
        fill(&mut a.s3_host_link, &b.s3_host_link);
        fill(&mut a.s3_host_public_key, &b.s3_host_public_key);
        fill(&mut a.s3_host_secret_key, &b.s3_host_secret_key);

        a.deprecated = b.deprecated;

        fill(&mut a.executable_file_name, &b.executable_file_name);

        if a.exception_names.is_empty() {
            a.exception_names = b.exception_names.clone();
        }
        if a.additional_file_names.is_empty() {
            a.additional_file_names = b.additional_file_names.clone();
        }

        a.replaces_original_game_file |= b.replaces_original_game_file;
        a.replaces_original_wb_file |= b.replaces_original_wb_file;

        if b.colors_information.is_some() {
            a.colors_information = b.colors_information.clone();
        }
    }
}

fn fill(target: &mut String, source: &str) {
    if target.is_empty() && !source.is_empty() {
        *target = source.to_owned();
    }
}

/// Orders two version strings the way `ModificationVersion.CompareTo` does:
/// keep only digits, right-pad the shorter with zeros, compare numerically.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let mut da: String = a.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut db: String = b.chars().filter(|c| c.is_ascii_digit()).collect();

    while da.len() > db.len() {
        db.push('0');
    }
    while da.len() < db.len() {
        da.push('0');
    }

    let parse = |s: &str| -> i128 {
        if s.is_empty() {
            -1
        } else {
            s.parse::<i128>().unwrap_or(i128::MAX)
        }
    };

    parse(&da).cmp(&parse(&db))
}

/// A named modification with all of its known versions. Mirrors `GameModification`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GameModification {
    /// Aggregated metadata across versions; the C# class inherits these fields.
    #[serde(flatten)]
    pub latest: ModVersion,
    #[serde(rename = "ModificationVersions")]
    pub versions: Vec<ModVersion>,
    #[serde(rename = "NumberInList")]
    pub number_in_list: i32,
}

impl GameModification {
    pub fn new(version: &ModVersion) -> Self {
        let mut m = GameModification {
            latest: ModVersion {
                info: ReposVersion {
                    name: version.info.name.clone(),
                    dependence_name: version.info.dependence_name.clone(),
                    ..Default::default()
                },
                ..Default::default()
            },
            versions: Vec::new(),
            number_in_list: 0,
        };
        m.update_with(version);
        m
    }

    pub fn name(&self) -> &str {
        &self.latest.info.name
    }
    pub fn kind(&self) -> ModificationType {
        self.latest.kind()
    }
    pub fn dependence_name(&self) -> &str {
        &self.latest.info.dependence_name
    }
    pub fn is_selected(&self) -> bool {
        self.latest.is_selected
    }
    pub fn set_selected(&mut self, selected: bool) {
        self.latest.is_selected = selected;
    }

    pub fn update_with(&mut self, version: &ModVersion) {
        match self.versions.iter_mut().find(|v| v.same_identity(version)) {
            Some(existing) => {
                existing.union(version);
                if self.latest.kind() == ModificationType::Advertising {
                    let a = &mut self.latest.info;
                    let b = &version.info;
                    a.mod_db_link = b.mod_db_link.clone();
                    a.network_info = b.network_info.clone();
                    a.discord_link = b.discord_link.clone();
                    a.simple_download_link = b.simple_download_link.clone();
                    a.support_link = b.support_link.clone();
                }
            }
            None => self.versions.push(version.clone()),
        }

        if !self.latest.installed && version.installed {
            self.latest.installed = true;
        }
        self.latest.union(version);
    }

    /// Versions ordered oldest to newest, as `OrderBy(m => m)` does in C#.
    pub fn versions_ordered(&self) -> Vec<&ModVersion> {
        let mut v: Vec<&ModVersion> = self.versions.iter().collect();
        v.sort_by(|a, b| compare_versions(a.version(), b.version()));
        v
    }

    pub fn latest_version(&self) -> Option<&ModVersion> {
        self.versions_ordered().last().copied()
    }

    pub fn latest_installed_version(&self) -> Option<&ModVersion> {
        self.versions_ordered().into_iter().rfind(|v| v.installed)
    }

    /// The version the user picked, falling back the way `GetSelectedVersion` does.
    pub fn display_version(&self) -> Option<&ModVersion> {
        if let Some(v) = self.versions.iter().find(|v| v.installed && v.is_selected) {
            return Some(v);
        }
        let ordered = self.versions_ordered();
        if let Some(v) = ordered.iter().copied().find(|v| v.installed) {
            return Some(v);
        }
        ordered.first().copied()
    }

    pub fn remove_version(&mut self, version: &ModVersion) {
        self.versions.retain(|v| !v.same_identity(version));
    }
}

/// How the game runs through Proton on Linux (see `game::proton`). Empty
/// fields mean "do what Steam does".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ProtonSettings {
    /// A Proton folder, or its `proton` script, to use instead of the one
    /// Steam runs the game with.
    #[serde(deserialize_with = "null_to_empty")]
    pub proton: String,
    /// Extra environment, as whitespace-separated `KEY=VALUE` pairs.
    #[serde(deserialize_with = "null_to_empty")]
    pub env: String,
    /// Run the game inside a Wine virtual desktop the size of its resolution.
    /// The game ignores the mouse on a monitor that sits left of or above the
    /// primary one; inside a virtual desktop there is only one screen.
    pub virtual_desktop: bool,
}

impl Default for ProtonSettings {
    fn default() -> Self {
        Self { proton: String::new(), env: String::new(), virtual_desktop: true }
    }
}

impl ProtonSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The persisted launcher config: `GenLauncherCfg.yaml`. Mirrors `LauncherData`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct LauncherData {
    pub modded_exe: bool,
    pub windowed: bool,
    pub quick_start: bool,
    pub camera_height: i32,
    pub launches_count: i32,
    pub auto_update_gentool: bool,
    pub auto_delete_old_versions: bool,
    #[serde(deserialize_with = "null_to_empty")]
    pub game_params: String,
    pub check_mod_files: bool,
    pub ask_before_check: bool,
    pub hide_launcher_after_game_start: bool,
    pub first_start: bool,
    pub use_vulkan: bool,
    /// How Proton runs the game on Linux. Left out of configs written on
    /// Windows, where it is never used. Configs from the Wine-based builds
    /// used `Wine:`; its `Env` and `VirtualDesktop` carry over.
    #[serde(alias = "Wine", skip_serializing_if = "ProtonSettings::is_default")]
    pub proton: ProtonSettings,

    pub modifications: Vec<GameModification>,
    pub addons: Vec<GameModification>,
    pub patches: Vec<GameModification>,
    pub exes: Vec<GameModification>,
}

impl Default for LauncherData {
    fn default() -> Self {
        Self {
            modded_exe: true,
            windowed: true,
            quick_start: true,
            camera_height: 0,
            launches_count: 0,
            auto_update_gentool: true,
            auto_delete_old_versions: false,
            game_params: String::new(),
            check_mod_files: true,
            ask_before_check: true,
            hide_launcher_after_game_start: false,
            first_start: true,
            use_vulkan: false,
            proton: ProtonSettings::default(),
            modifications: Vec::new(),
            addons: Vec::new(),
            patches: Vec::new(),
            exes: Vec::new(),
        }
    }
}

impl LauncherData {
    fn storage_mut(&mut self, kind: ModificationType) -> &mut Vec<GameModification> {
        match kind {
            ModificationType::Mod | ModificationType::Advertising => &mut self.modifications,
            ModificationType::Addon => &mut self.addons,
            ModificationType::Patch => &mut self.patches,
            ModificationType::Executable => &mut self.exes,
        }
    }

    pub fn add_or_update(&mut self, version: &ModVersion) {
        if version.kind() == ModificationType::Addon && version.info.dependence_name.is_empty() {
            return;
        }
        let storage = self.storage_mut(version.kind());
        match storage.iter_mut().find(|m| m.name().eq_ignore_ascii_case(version.name())) {
            Some(existing) => existing.update_with(version),
            None => storage.push(GameModification::new(version)),
        }
    }

    pub fn delete(&mut self, version: &ModVersion) {
        // Deleting a mod also drops its addons and patches, as in C#.
        let lists: Vec<ModificationType> = match version.kind() {
            ModificationType::Mod => {
                vec![ModificationType::Mod, ModificationType::Addon, ModificationType::Patch]
            }
            other => vec![other],
        };

        for kind in lists {
            let storage = self.storage_mut(kind);
            if let Some(idx) =
                storage.iter().position(|m| m.name().eq_ignore_ascii_case(version.name()))
            {
                storage[idx].remove_version(version);
                if storage[idx].versions.is_empty() {
                    storage.remove(idx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquoted_numeric_versions_survive_as_written() {
        // YamlDotNet turned these scalars into strings; so must we, because the
        // version doubles as the on-disk folder name.
        let yaml = r#"
Name: Contra
Version: 1.86
"#;
        let v: ReposVersion = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(v.version, "1.86");

        let yaml = "Name: Contra
Version: 2.0
";
        let v: ReposVersion = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(v.version, "2.0", "a trailing .0 must not be dropped");

        let yaml = "Name: Contra
Version: 009
";
        let v: ReposVersion = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(v.version, "009");

        let yaml = "Name: Contra
Version: 1.87 Public Build 2.0
";
        let v: ReposVersion = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(v.version, "1.87 Public Build 2.0");
    }

    #[test]
    fn null_scalars_read_as_empty() {
        let v: ReposVersion = serde_yaml_ng::from_str("Name: X
DependenceName:
").unwrap();
        assert!(v.dependence_name.is_empty());
    }

    #[test]
    fn a_numeric_version_round_trips_through_serialization() {
        let mut data = LauncherData::default();
        data.add_or_update(&ModVersion {
            info: ReposVersion {
                name: "Contra".into(),
                version: "2.0".into(),
                ..Default::default()
            },
            installed: true,
            is_selected: true,
        });

        let yaml = serde_yaml_ng::to_string(&data).unwrap();
        let back: LauncherData = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.modifications[0].versions[0].version(), "2.0");
        assert!(back.modifications[0].is_selected(), "selection must survive a save");
    }

    #[test]
    fn version_ordering_matches_the_csharp_comparator() {
        assert_eq!(compare_versions("1.86", "1.87"), Ordering::Less);
        assert_eq!(compare_versions("2.0", "2.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0", "1.0.1"), Ordering::Less);

        // The comparator strips non-digits and right-pads the shorter operand
        // with zeros, so "1.9" is read as "190" and outranks "1.10" ("110").
        // Quirky, but it is what the C# build shipped and what mod authors
        // have been numbering against.
        assert_eq!(compare_versions("1.9", "1.10"), Ordering::Greater);

        // No digits at all sorts below everything.
        assert_eq!(compare_versions("beta", "1.0"), Ordering::Less);
    }
}

