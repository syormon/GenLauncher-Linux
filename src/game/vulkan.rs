//! The optional Vulkan translation layer. Port of `VulkanDllsHandler`.

use crate::config;
use crate::game::symlinks;
use crate::model::repos::VulkanData;
use crate::util::fs as gfs;
use crate::util::pe_version;

const INTEGRATION_DLL: &str = "d3d8x.dll";

pub fn folder() -> std::path::PathBuf {
    config::game_path(config::VULKAN_DLLS_FOLDER_NAME)
}

/// Version of the installed integration DLL, or `-1` when it is missing.
pub fn installed_version() -> String {
    let dll = folder().join(INTEGRATION_DLL);
    pe_version::file_version(&dll).unwrap_or_else(|| "-1".to_owned())
}

/// True when the repository advertises a newer build than the installed one.
pub fn is_outdated(data: Option<&VulkanData>) -> bool {
    let Some(data) = data else { return false };
    if data.latest_version.is_empty() {
        return false;
    }
    crate::util::version_is_older(&installed_version(), &data.latest_version)
}

/// Link the Vulkan DLLs into the game folder.
///
/// With GenTool auto-update on, every DLL is linked under its own name and
/// GenTool keeps `d3d8.dll`. With it off, the integration DLL takes that slot.
pub fn create_symlinks(gentool_auto_update: bool) {
    let dir = folder();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };

    for entry in entries.flatten() {
        let source = entry.path();
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }

        let name = gfs::file_name_of(&source);
        let link_name = if !gentool_auto_update && name.eq_ignore_ascii_case(INTEGRATION_DLL) {
            "d3d8.dll".to_owned()
        } else {
            name
        };

        if let Err(e) =
            symlinks::create_mirror_for_non_big(&source, &config::game_path(&link_name))
        {
            log::warn!("could not link Vulkan dll {link_name}: {e}");
        }
    }
}
