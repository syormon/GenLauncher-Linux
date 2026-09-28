//! Launcher state: the persisted config plus whatever the repositories told us
//! this session. Replaces the static `DataHandler` class.

use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

use crate::config::{self, Game};
use crate::model::repos::{ModAddonsAndPatchesRawData, ReposModsData, VulkanData};
use crate::model::{GameModification, LauncherData, ModVersion, ModificationType, ReposVersion};
use crate::util::fs as gfs;

/// What the GitHub manifests told us this session.
#[derive(Debug, Default)]
pub struct Repos {
    pub index: ReposModsData,
    /// Every mod the repository offers, in listing order.
    pub mod_names: Vec<String>,
    pub vulkan: Option<VulkanData>,
    /// Mods whose patch/addon manifests we have already pulled.
    loaded_details: HashSet<String>,
    /// Versions that exist in a repository, so a missing local folder means
    /// "not installed" rather than "forget this modification".
    from_repos: Vec<ModVersion>,
}

impl Repos {
    pub fn raw_data_for(&self, mod_name: &str) -> Option<&ModAddonsAndPatchesRawData> {
        self.index.mod_datas.iter().find(|m| m.mod_name.eq_ignore_ascii_case(mod_name))
    }

    pub fn mark_details_loaded(&mut self, mod_name: &str) -> bool {
        self.loaded_details.insert(mod_name.to_ascii_lowercase())
    }

    pub fn details_loaded(&self, mod_name: &str) -> bool {
        self.loaded_details.contains(&mod_name.to_ascii_lowercase())
    }

    pub fn note_repos_version(&mut self, version: &ModVersion) {
        if !self.from_repos.iter().any(|v| v.same_identity(version)) {
            self.from_repos.push(version.clone());
        }
    }

    fn is_from_repos(&self, version: &ModVersion) -> bool {
        self.from_repos.iter().any(|v| v.same_identity(version))
    }
}

pub struct Store {
    pub data: LauncherData,
    pub repos: Repos,
    pub connected: bool,
    pub game_mode: Game,
}

impl Store {
    pub fn new(game_mode: Game) -> Self {
        Self {
            data: load_config(),
            repos: Repos::default(),
            connected: false,
            game_mode,
        }
    }

    // -- selection -------------------------------------------------------

    pub fn selected_mod(&self) -> Option<&GameModification> {
        self.data.modifications.iter().find(|m| m.is_selected())
    }

    pub fn selected_mod_name(&self) -> Option<String> {
        self.selected_mod().map(|m| m.name().to_owned())
    }

    pub fn selected_mod_version(&self) -> Option<&ModVersion> {
        self.selected_mod()?.versions.iter().find(|v| v.is_selected)
    }

    /// Name the patch and addon tabs hang off: the selected mod, else the game.
    pub fn dependency_name(&self) -> String {
        self.selected_mod_name().unwrap_or_else(|| config::ORIGINAL_GAME_ALIAS.to_owned())
    }

    pub fn unselect_all_mods(&mut self) {
        for m in &mut self.data.modifications {
            m.set_selected(false);
        }
    }

    pub fn patches_for_selected_mod(&self) -> Vec<&GameModification> {
        let dependency = self.dependency_name();
        self.data
            .patches
            .iter()
            .filter(|m| m.dependence_name().eq_ignore_ascii_case(&dependency))
            .collect()
    }

    pub fn selected_patch(&self) -> Option<&GameModification> {
        self.patches_for_selected_mod().into_iter().find(|m| m.is_selected())
    }

    pub fn selected_patch_version(&self) -> Option<&ModVersion> {
        self.selected_patch()?.versions.iter().find(|v| v.is_selected)
    }

    /// Addons attach either to the selected mod or to the selected patch.
    pub fn addons_for_selected_mod(&self) -> Vec<&GameModification> {
        let dependency = self.dependency_name();
        let patch_name = self.selected_patch().map(|p| p.name().to_owned());

        self.data
            .addons
            .iter()
            .filter(|m| {
                m.dependence_name().eq_ignore_ascii_case(&dependency)
                    || patch_name
                        .as_deref()
                        .is_some_and(|p| m.dependence_name().eq_ignore_ascii_case(p))
            })
            .collect()
    }

    /// Executables for the selected mod, plus the game-wide ones.
    pub fn exes_for_selected_mod(&self) -> Vec<&GameModification> {
        let selected = self.selected_mod_name();
        self.data
            .exes
            .iter()
            .filter(|m| {
                let dep = m.dependence_name();
                let matches_mod =
                    selected.as_deref().is_some_and(|s| dep.eq_ignore_ascii_case(s));
                matches_mod
                    || dep.eq_ignore_ascii_case(config::ORIGINAL_GAME_ALIAS)
                    || dep.is_empty()
            })
            .collect()
    }

    pub fn selected_addon_versions(&self) -> Vec<ModVersion> {
        self.addons_for_selected_mod()
            .into_iter()
            .filter(|m| m.is_selected())
            .filter_map(|m| m.versions.iter().find(|v| v.is_selected).cloned())
            .collect()
    }

    pub fn selected_exe_versions(&self) -> Vec<ModVersion> {
        self.exes_for_selected_mod()
            .into_iter()
            .filter(|m| m.is_selected())
            .filter_map(|m| m.versions.iter().find(|v| v.is_selected).cloned())
            .collect()
    }

    /// Everything that has to be linked in before the game starts.
    pub fn active_versions(&self) -> Vec<ModVersion> {
        let mut versions = Vec::new();
        versions.extend(self.selected_mod_version().cloned());
        versions.extend(self.selected_patch_version().cloned());
        versions.extend(self.selected_addon_versions());
        versions.extend(self.selected_exe_versions());
        versions
    }

    /// Mutable access to one modification by kind and name.
    pub fn modification_mut(
        &mut self,
        kind: ModificationType,
        name: &str,
    ) -> Option<&mut GameModification> {
        let list = match kind {
            ModificationType::Mod | ModificationType::Advertising => &mut self.data.modifications,
            ModificationType::Addon => &mut self.data.addons,
            ModificationType::Patch => &mut self.data.patches,
            ModificationType::Executable => &mut self.data.exes,
        };
        list.iter_mut().find(|m| m.name().eq_ignore_ascii_case(name))
    }

    pub fn modification(&self, kind: ModificationType, name: &str) -> Option<&GameModification> {
        let list = match kind {
            ModificationType::Mod | ModificationType::Advertising => &self.data.modifications,
            ModificationType::Addon => &self.data.addons,
            ModificationType::Patch => &self.data.patches,
            ModificationType::Executable => &self.data.exes,
        };
        list.iter().find(|m| m.name().eq_ignore_ascii_case(name))
    }

    /// Mark exactly one version of a modification as the chosen one.
    pub fn select_version(&mut self, kind: ModificationType, name: &str, version: &str) {
        if let Some(m) = self.modification_mut(kind, name) {
            for v in &mut m.versions {
                v.is_selected = v.info.version.eq_ignore_ascii_case(version);
            }
        }
    }

    // -- local folder scanning -------------------------------------------

    /// Re-read `GLM/` and reconcile it with the config: register anything the
    /// user dropped in by hand and forget anything that has gone.
    pub fn refresh_local_modifications(&mut self) {
        self.add_unregistered_modifications();
        self.drop_missing_modifications();
    }

    fn add_unregistered_modifications(&mut self) {
        let root = config::game_path(config::MODS_FOLDER);
        let Ok(entries) = std::fs::read_dir(&root) else { return };

        for mod_dir in entries.flatten() {
            if !mod_dir.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let mod_name = gfs::file_name_of(&mod_dir.path());
            let Ok(children) = std::fs::read_dir(mod_dir.path()) else { continue };

            for child in children.flatten() {
                if !child.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let child_name = gfs::file_name_of(&child.path());

                let nested_kind = match child_name.as_str() {
                    config::ADDONS_FOLDER_NAME => Some(ModificationType::Addon),
                    config::PATCHES_FOLDER_NAME => Some(ModificationType::Patch),
                    config::EXECUTABLES_FOLDER_NAME => Some(ModificationType::Executable),
                    _ => None,
                };

                if let Some(kind) = nested_kind {
                    self.scan_nested(&child.path(), &mod_name, kind);
                    continue;
                }

                // A direct child folder is a version of the mod itself.
                if self.is_installed_version_folder(&child.path()) {
                    let version = ModVersion {
                        info: ReposVersion {
                            name: mod_name.clone(),
                            version: child_name,
                            modification_type: ModificationType::Mod,
                            ..Default::default()
                        },
                        installed: true,
                        is_selected: false,
                    };
                    self.data.add_or_update(&version);
                }
            }
        }
    }

    fn scan_nested(&mut self, dir: &Path, dependency: &str, kind: ModificationType) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for item in entries.flatten() {
            if !item.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = gfs::file_name_of(&item.path());
            let Ok(versions) = std::fs::read_dir(item.path()) else { continue };

            for version_dir in versions.flatten() {
                if !version_dir.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                if !self.is_installed_version_folder(&version_dir.path()) {
                    continue;
                }
                let version = ModVersion {
                    info: ReposVersion {
                        name: name.clone(),
                        version: gfs::file_name_of(&version_dir.path()),
                        modification_type: kind,
                        dependence_name: dependency.to_owned(),
                        ..Default::default()
                    },
                    installed: true,
                    is_selected: false,
                };
                self.data.add_or_update(&version);
            }
        }
    }

    /// A version folder counts as installed when it holds files and is not a
    /// half-finished download.
    fn is_installed_version_folder(&self, path: &Path) -> bool {
        !gfs::file_name_of(path).contains(config::VERSION_FOLDER_COPY_SUFFIX)
            && gfs::folder_contains_files(path)
    }

    /// Drop versions whose folder no longer exists, or mark them uninstalled
    /// when the repository still offers them.
    fn drop_missing_modifications(&mut self) {
        let mut to_forget: Vec<ModVersion> = Vec::new();
        let mut to_mark_uninstalled: Vec<ModVersion> = Vec::new();

        let lists = [
            &self.data.modifications,
            &self.data.addons,
            &self.data.patches,
            &self.data.exes,
        ];

        for list in lists {
            for modification in list.iter() {
                for version in &modification.versions {
                    if version.kind() == ModificationType::Advertising {
                        continue;
                    }
                    let folder = version.folder_path();
                    if folder.is_dir() && gfs::folder_contains_files(&folder) {
                        continue;
                    }
                    if self.repos.is_from_repos(version) {
                        to_mark_uninstalled.push(version.clone());
                    } else {
                        to_forget.push(version.clone());
                    }
                }
            }
        }

        for version in &to_mark_uninstalled {
            if let Some(m) = self.modification_mut(version.kind(), version.name()) {
                if let Some(v) = m.versions.iter_mut().find(|v| v.same_identity(version)) {
                    v.installed = false;
                }
                m.latest.installed = m.versions.iter().any(|v| v.installed);
            }
        }

        for version in &to_forget {
            self.data.delete(version);
        }
    }

    // -- repository merges -----------------------------------------------

    /// Register a version the repository advertised.
    pub fn add_repos_version(&mut self, info: ReposVersion) {
        let version = ModVersion::from(info);
        self.data.add_or_update(&version);
        self.repos.note_repos_version(&version);
    }

    /// Names the user has not added yet, for the "add mod" dialog.
    pub fn addable_mod_names(&self) -> Vec<String> {
        let installed: Vec<String> =
            self.data.modifications.iter().map(|m| m.name().to_ascii_lowercase()).collect();
        self.repos
            .mod_names
            .iter()
            .filter(|n| !installed.contains(&n.to_ascii_lowercase()))
            .cloned()
            .collect()
    }

    // -- deletion ---------------------------------------------------------

    /// Drop any advertising entry a config written by the C# launcher still
    /// carries. The advert was injected into the mod list as a pseudo-mod; this
    /// build does not show one, so it is removed rather than left orphaned.
    pub fn purge_advertising(&mut self) {
        self.data.modifications.retain(|m| m.kind() != ModificationType::Advertising);
    }

    /// Delete one installed version from disk, then refresh the config.
    pub fn delete_version(&mut self, version: &ModVersion) -> Result<()> {
        if version.kind() == ModificationType::Advertising {
            self.data.delete(version);
            return Ok(());
        }

        let folder = version.folder_path();
        if folder.exists() {
            std::fs::remove_dir_all(&folder)?;
        }
        self.refresh_local_modifications();
        Ok(())
    }

    // -- persistence -------------------------------------------------------

    pub fn save(&self) {
        if let Err(e) = save_config(&self.data) {
            log::error!("could not save launcher config: {e:#}");
        }
    }
}

/// Create `.GenLauncherFolder`, hidden on Windows as the C# build did.
pub fn create_launcher_folder() -> Result<()> {
    let folder = config::game_path(config::LAUNCHER_FOLDER);
    if folder.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&folder)?;

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> =
            folder.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated path that outlives the call.
        unsafe {
            windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(
                wide.as_ptr(),
                windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN,
            );
        }
    }

    Ok(())
}

pub fn load_config() -> LauncherData {
    let path = config::game_path(config::CONFIG_NAME);
    if !path.exists() {
        return LauncherData::default();
    }

    match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|text| {
        serde_yaml_ng::from_str::<LauncherData>(&text).map_err(anyhow::Error::from)
    }) {
        Ok(data) => data,
        Err(e) => {
            // A config we cannot read is a config we cannot trust; start over.
            log::warn!("discarding unreadable config ({e:#})");
            let _ = std::fs::remove_file(&path);
            LauncherData::default()
        }
    }
}

pub fn save_config(data: &LauncherData) -> Result<()> {
    create_launcher_folder()?;
    let path = config::game_path(config::CONFIG_NAME);
    let yaml = serde_yaml_ng::to_string(data)?;
    std::fs::write(path, yaml)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(name: &str, version: &str, kind: ModificationType, dep: &str) -> ModVersion {
        ModVersion {
            info: ReposVersion {
                name: name.into(),
                version: version.into(),
                modification_type: kind,
                dependence_name: dep.into(),
                ..Default::default()
            },
            installed: true,
            is_selected: false,
        }
    }

    #[test]
    fn addons_follow_the_selected_mod_and_its_patch() {
        let mut store = Store::new(Game::ZeroHour);
        store.data.add_or_update(&version("ROTR", "1.87", ModificationType::Mod, ""));
        store.data.add_or_update(&version("HanPatch", "2.0", ModificationType::Patch, "ROTR"));
        store.data.add_or_update(&version("HUD", "1.0", ModificationType::Addon, "ROTR"));
        store.data.add_or_update(&version("EggHunt", "1.0", ModificationType::Addon, "HanPatch"));
        store.data.add_or_update(&version("Other", "1.0", ModificationType::Addon, "Contra"));

        store.data.modifications[0].set_selected(true);
        assert_eq!(store.dependency_name(), "ROTR");

        // Without a patch selected, only the mod's own addons show.
        let names: Vec<&str> =
            store.addons_for_selected_mod().iter().map(|m| m.name()).collect();
        assert_eq!(names, vec!["HUD"]);

        store.data.patches[0].set_selected(true);
        let mut names: Vec<&str> =
            store.addons_for_selected_mod().iter().map(|m| m.name()).collect();
        names.sort();
        assert_eq!(names, vec!["EggHunt", "HUD"]);
    }

    #[test]
    fn with_no_mod_selected_the_original_game_is_the_dependency() {
        let mut store = Store::new(Game::ZeroHour);
        store.data.add_or_update(&version(
            "1.04 Patch",
            "1.0",
            ModificationType::Patch,
            config::ORIGINAL_GAME_ALIAS,
        ));
        assert_eq!(store.dependency_name(), config::ORIGINAL_GAME_ALIAS);
        assert_eq!(store.patches_for_selected_mod().len(), 1);
    }

    #[test]
    fn active_versions_gather_every_selection() {
        let mut store = Store::new(Game::ZeroHour);
        store.data.add_or_update(&version("ROTR", "1.87", ModificationType::Mod, ""));
        store.data.add_or_update(&version("HUD", "1.0", ModificationType::Addon, "ROTR"));

        store.data.modifications[0].set_selected(true);
        store.data.modifications[0].versions[0].is_selected = true;
        store.data.addons[0].set_selected(true);
        store.data.addons[0].versions[0].is_selected = true;

        let active = store.active_versions();
        assert_eq!(active.len(), 2);
        assert!(active.iter().any(|v| v.name() == "ROTR"));
        assert!(active.iter().any(|v| v.name() == "HUD"));
    }

    #[test]
    fn config_round_trips_through_yaml() {
        let mut data = LauncherData {
            camera_height: 300,
            game_params: "-noaudio".into(),
            ..Default::default()
        };
        data.add_or_update(&version("ROTR", "1.87", ModificationType::Mod, ""));

        let yaml = serde_yaml_ng::to_string(&data).unwrap();
        // Keys must stay PascalCase so old C# configs keep loading.
        assert!(yaml.contains("CameraHeight"), "{yaml}");
        assert!(yaml.contains("ModificationVersions"), "{yaml}");

        let back: LauncherData = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.camera_height, 300);
        assert_eq!(back.modifications.len(), 1);
        assert_eq!(back.modifications[0].name(), "ROTR");
    }

    #[test]
    fn reads_the_wine_section_of_older_linux_configs() {
        let yaml = "Windowed: true\nWine:\n  Binary: /usr/bin/wine\n  Prefix: ~/.wine\n  Env: DXVK_HUD=fps\n  VirtualDesktop: false\n";
        let data: LauncherData = serde_yaml_ng::from_str(yaml).unwrap();
        // The Wine binary and prefix no longer apply; the rest carries over.
        assert_eq!(data.proton.proton, "");
        assert_eq!(data.proton.env, "DXVK_HUD=fps");
        assert!(!data.proton.virtual_desktop);

        let yaml = serde_yaml_ng::to_string(&data).unwrap();
        assert!(yaml.contains("Proton:") && !yaml.contains("Wine:"), "{yaml}");
        // Nothing to write when every Proton setting is left at its default.
        let yaml = serde_yaml_ng::to_string(&LauncherData::default()).unwrap();
        assert!(!yaml.contains("Proton:"), "{yaml}");
    }

    #[test]
    fn reads_a_config_written_by_the_csharp_build() {
        let yaml = r#"
ModdedExe: true
Windowed: false
QuickStart: true
CameraHeight: 450
LaunchesCount: 12
AutoUpdateGentool: true
AutoDeleteOldVersions: false
GameParams:
CheckModFiles: true
AskBeforeCheck: true
HideLauncherAfterGameStart: false
FirstStart: false
UseVulkan: false
Modifications:
- ModificationVersions:
  - IsSelected: true
    Installed: true
    ModificationType: Mod
    Name: Rise of the Reds
    Version: 1.87 Public Build 2.0
    S3BucketName: rotr
  NumberInList: 0
  IsSelected: true
  Installed: true
  ModificationType: Mod
  Name: Rise of the Reds
  Version: 1.87 Public Build 2.0
Addons: []
Patches: []
Exes: []
"#;
        let data: LauncherData = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(data.camera_height, 450);
        assert!(!data.windowed);
        assert!(data.game_params.is_empty(), "a null GameParams must read as empty");
        assert_eq!(data.modifications.len(), 1);
        let rotr = &data.modifications[0];
        assert_eq!(rotr.name(), "Rise of the Reds");
        assert!(rotr.is_selected());
        assert_eq!(rotr.versions.len(), 1);
        assert_eq!(rotr.versions[0].info.s3_bucket_name, "rotr");
    }


    #[test]
    fn saved_order_survives_a_reload() {
        // `number_in_list` is what persists the user's drag-and-drop order, and
        // the list is re-sorted by it on load so row indices stay meaningful.
        let mut data = LauncherData::default();
        for name in ["Alpha", "Beta", "Gamma"] {
            data.add_or_update(&version(name, "1.0", ModificationType::Mod, ""));
        }
        // Pretend the user dragged Gamma to the top.
        data.modifications[0].number_in_list = 1;
        data.modifications[1].number_in_list = 2;
        data.modifications[2].number_in_list = 0;

        let yaml = serde_yaml_ng::to_string(&data).unwrap();
        let mut back: LauncherData = serde_yaml_ng::from_str(&yaml).unwrap();
        back.modifications.sort_by_key(|m| m.number_in_list);

        let names: Vec<&str> = back.modifications.iter().map(|m| m.name()).collect();
        assert_eq!(names, vec!["Gamma", "Alpha", "Beta"]);
    }

    #[test]
    fn an_advert_saved_by_the_csharp_launcher_is_dropped() {
        // The C# build injected its advert into the mod list and persisted it,
        // so a migrated config carries one. It must not show up as a mod card.
        let yaml = r#"
Modifications:
- ModificationVersions:
  - IsSelected: false
    Installed: false
    ModificationType: Advertising
    Name: Do you like GenLauncher?
    Version: '1'
  NumberInList: 0
  ModificationType: Advertising
  Name: Do you like GenLauncher?
- ModificationVersions:
  - IsSelected: true
    Installed: true
    ModificationType: Mod
    Name: Contra
    Version: '009'
  NumberInList: 1
  IsSelected: true
  Installed: true
  ModificationType: Mod
  Name: Contra
Addons: []
Patches: []
Exes: []
"#;
        let data: LauncherData = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(data.modifications.len(), 2, "the advert should still parse");

        let mut store = Store::new(Game::ZeroHour);
        store.data = data;
        store.purge_advertising();

        let names: Vec<&str> = store.data.modifications.iter().map(|m| m.name()).collect();
        assert_eq!(names, vec!["Contra"], "only the real mod should remain");
    }
}

