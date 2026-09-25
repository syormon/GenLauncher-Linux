//! GenTool version handling. Port of `GentoolHandler`.
//! The HTTP calls live in `net::gentool`; this module is the pure logic.
//!
//! Note: the C# build defined `CheckAndUpdateGentool` but never called it, so
//! the auto-update the options screen advertises has not actually run since
//! that method was orphaned. The check is ported here so it can be wired up,
//! but it is deliberately left uncalled to keep this migration behaviour-for-
//! behaviour with the original.
#![allow(dead_code)]

use std::path::Path;

use crate::config;
use crate::util::pe_version;

pub const GENTOOL_SITE: &str = "https://www.gentool.net/";
pub const GENTOOL_SITE_HTTP: &str = "http://www.gentool.net/";

const GENTOOL_DLL: &str = "d3d8.dll";
const GENTOOL_CFG: &str = "d3d8.cfg";
const DEFAULT_CFG: &str = include_str!("../../assets/d3d8.cfg");

/// The version GenTool's own `d3d8.dll` reports, or -1 when it is absent.
pub fn current_version() -> i64 {
    let dll = config::game_path(GENTOOL_DLL);
    pe_version::product_version(&dll)
        .and_then(|v| digits_only(&v).parse::<i64>().ok())
        .unwrap_or(-1)
}

/// Pull `gentool_ver` out of the landing page's inline script.
pub fn parse_latest_version(html: &str) -> Option<String> {
    let line = html.lines().find(|l| l.contains("var gentool_ver"))?;
    let version: String = line.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
    (!version.is_empty()).then_some(version)
}

/// `7.4.2` compares as `742`.
pub fn version_to_int(version: &str) -> i64 {
    digits_only(version).parse().unwrap_or(-1)
}

pub fn is_outdated(current: i64, latest: &str) -> bool {
    let latest = version_to_int(latest);
    latest > 0 && current < latest
}

pub fn download_link(latest_version: &str) -> String {
    format!("http://www.gentool.net/download/GenTool_v{latest_version}.zip")
}

fn digits_only(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Force GenTool's borderless-window mode, which the recommended settings use.
pub fn set_recommended_window_options() -> std::io::Result<()> {
    let cfg = config::game_path(GENTOOL_CFG);

    if !cfg.exists() {
        return std::fs::write(&cfg, DEFAULT_CFG);
    }

    let text = std::fs::read_to_string(&cfg)?;
    let patched: Vec<String> = text
        .lines()
        .map(|line| {
            if line.contains("window =") {
                "window=3".to_owned()
            } else {
                line.to_owned()
            }
        })
        .collect();

    std::fs::write(&cfg, patched.join("\n") + "\n")
}

/// Write the stock `d3d8.cfg` if the game folder has none.
pub fn extract_default_cfg() -> std::io::Result<()> {
    let cfg = config::game_path(GENTOOL_CFG);
    if cfg.exists() {
        return Ok(());
    }
    std::fs::write(cfg, DEFAULT_CFG)
}

/// Path of GenTool's DLL inside the game folder.
pub fn dll_path() -> std::path::PathBuf {
    config::game_path(GENTOOL_DLL)
}

/// Move GenTool's DLL out of the way (used when the user disables it).
pub fn shadow_dll() {
    let dll = dll_path();
    if dll.exists() {
        let shadowed = Path::new(GENTOOL_DLL).with_extension("");
        let _ = shadowed; // name kept only for clarity
        let target =
            config::game_path(format!("{GENTOOL_DLL}{}", config::REPLACE_SUFFIX));
        let _ = std::fs::remove_file(&target);
        let _ = crate::util::fs::move_file(&dll, &target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_version_from_the_landing_page() {
        let html = "<script>\n var gentool_ver = \"7.4.2\";\n</script>";
        assert_eq!(parse_latest_version(html).as_deref(), Some("7.4.2"));
        assert_eq!(parse_latest_version("<html></html>"), None);
    }

    #[test]
    fn compares_versions_as_packed_digits() {
        assert_eq!(version_to_int("7.4.2"), 742);
        assert!(is_outdated(741, "7.4.2"));
        assert!(!is_outdated(742, "7.4.2"));
        assert!(!is_outdated(743, "7.4.2"));
    }
}
