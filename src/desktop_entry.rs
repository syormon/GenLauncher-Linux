//! Desktop integration on Linux.
//!
//! A Wayland window has no icon of its own: the compositor looks up the
//! `.desktop` file whose name matches the window's app id and shows its
//! `Icon`. So on start-up the launcher writes its icon and a desktop entry
//! into the user's data folder (`~/.local/share`), which also puts GenLauncher
//! in the application menu. Files are only rewritten when their content changed.
#![cfg_attr(windows, allow(dead_code))]

use std::path::{Path, PathBuf};

/// The window's app id, and the desktop entry's file name.
pub const APP_ID: &str = "genlauncher";

#[cfg(not(windows))]
pub fn install(icon_png: &[u8]) {
    let Some(data) = dirs::data_dir() else { return };
    let icon = data.join("icons/hicolor/256x256/apps").join(format!("{APP_ID}.png"));
    let entry = data.join("applications").join(format!("{APP_ID}.desktop"));

    let Ok(exe) = std::env::current_exe() else { return };
    let text = desktop_entry(&exe, crate::config::game_dir(), &icon);

    for (path, bytes) in [(&icon, icon_png), (&entry, text.as_bytes())] {
        if let Err(e) = write_if_changed(path, bytes) {
            log::warn!("could not write {}: {e}", path.display());
        }
    }
}

/// The entry starts this binary on the game folder it is managing now, and
/// names the icon by absolute path so no icon-cache refresh is needed.
fn desktop_entry(exe: &Path, game_dir: &Path, icon: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=GenLauncher\n\
         Comment=Mod manager for Command & Conquer: Generals and Zero Hour\n\
         Exec={} {}\n\
         Path={}\n\
         Icon={}\n\
         Terminal=false\n\
         Categories=Game;\n\
         StartupWMClass={APP_ID}\n",
        exec_arg(exe),
        exec_arg(game_dir),
        game_dir.display(),
        icon.display(),
    )
}

/// Quote one `Exec` argument as the desktop entry spec requires: inside double
/// quotes, `"`, `` ` ``, `$` and `\` are escaped, and `%` is doubled.
fn exec_arg(path: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in path.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

fn write_if_changed(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_exec_arguments() {
        assert_eq!(
            exec_arg(Path::new("/games/Command & Conquer - Zero Hour")),
            "\"/games/Command & Conquer - Zero Hour\""
        );
        assert_eq!(exec_arg(Path::new("/a/100%/$x")), "\"/a/100%%/\\$x\"");
    }

    #[test]
    fn entry_names_the_window_class_and_icon() {
        let text = desktop_entry(Path::new("/opt/genlauncher"), Path::new("/zh"), Path::new("/i/genlauncher.png"));
        assert!(text.contains("Exec=\"/opt/genlauncher\" \"/zh\"\n"));
        assert!(text.contains("Icon=/i/genlauncher.png\n"));
        assert!(text.contains("StartupWMClass=genlauncher\n"));
    }
}
