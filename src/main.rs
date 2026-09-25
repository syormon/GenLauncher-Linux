//! GenLauncher — mod manager for Command & Conquer: Generals and Zero Hour.
//!
//! A Rust/egui port of the original WPF launcher. It keeps the same on-disk
//! layout (`GLM/`, `.GenLauncherFolder/GenLauncherCfg.yaml`) and the same
//! repository manifests, so an existing installation carries over unchanged.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod config;
mod game;
mod i18n;
mod model;
mod net;
mod state;
mod tasks;
mod ui;
mod util;

use app::{GenLauncherApp, Phase};
use config::Game;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("genlauncher=info,warn"),
    )
    .init();

    config::set_game_dir(std::env::current_dir().unwrap_or_else(|_| ".".into()));
    choose_language();

    let Some(game) = preflight() else {
        return Ok(());
    };

    // Only one launcher may touch the game folder at a time.
    let _guard = match SingleInstance::acquire() {
        Some(guard) => guard,
        None => {
            message_box("GenLauncher", "GenLauncher is already running.");
            return Ok(());
        }
    };

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(ui::theme::WINDOW_SIZE)
            .with_min_inner_size(egui::vec2(720.0, 520.0))
            .with_title("GenLauncher"),
        ..Default::default()
    };

    if let Some(icon) = load_icon() {
        options.viewport = options.viewport.with_icon(icon);
    }

    eframe::run_native(
        "GenLauncher",
        options,
        Box::new(move |cc| Ok(Box::new(Launcher::new(cc, game)))),
    )
}

/// Honour the `eng` marker file, else follow the OS language.
fn choose_language() {
    let marker = config::game_path(config::LAUNCHER_FOLDER).join("eng");
    if marker.is_file() {
        i18n::set_language("en");
    } else {
        i18n::set_language(i18n::system_language());
    }
}

/// The checks `EntryPoint.Main` ran before showing any window. Returns the
/// detected game, or `None` when the launcher must not start here.
fn preflight() -> Option<Game> {
    let dir = config::game_dir();

    let Some(game) = tasks::detect_game().filter(|_| looks_like_a_game_folder()) else {
        message_box("GenLauncher", &i18n::tr("MoveLauncher"));
        return None;
    };

    // A crash inside the game leaves this behind and breaks the next launch.
    let _ = std::fs::remove_file(config::game_path("dbghelp.dll"));

    if !util::fs::can_create_symlinks(dir) {
        message_box("GenLauncher", &i18n::tr("SymbLink"));
        return None;
    }

    Some(game)
}

/// A real install has the executable, the Bink codec and a window archive.
fn looks_like_a_game_folder() -> bool {
    let has = |name: &str| config::game_path(name).exists();
    let has_either = |a: &str, b: &str| has(a) || has(b);

    has("generals.exe")
        && has_either("BINKW32.DLL", "binkw32.dll")
        && (has_either("WindowZH.big", "Window.big")
            || has_either(
                &format!("WindowZH.big{}", config::REPLACE_SUFFIX),
                &format!("Window.big{}", config::REPLACE_SUFFIX),
            ))
}

fn message_box(title: &str, message: &str) {
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(message)
        .set_level(rfd::MessageLevel::Warning)
        .show();
}

/// The launcher's own icon, shipped with the original project as `fd.ico`.
/// Used for the title bar, the taskbar button and Alt+Tab.
const WINDOW_ICON: &[u8] = include_bytes!("../assets/fd.ico");

/// Decode `fd.ico`. The file holds 16/32/48px entries and the decoder picks the
/// largest. Returns `None` rather than a blank icon, so a failure leaves the
/// platform default in place instead of an empty square.
fn load_icon() -> Option<egui::IconData> {
    let image = image::load_from_memory_with_format(WINDOW_ICON, image::ImageFormat::Ico)
        .inspect_err(|e| log::warn!("could not decode the window icon: {e}"))
        .ok()?;

    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(egui::IconData { rgba: rgba.into_raw(), width, height })
}

/// A lock file in the game folder, so a second launcher cannot start there.
struct SingleInstance {
    path: std::path::PathBuf,
    _file: std::fs::File,
}

impl SingleInstance {
    fn acquire() -> Option<Self> {
        let path = config::game_path(config::LAUNCHER_FOLDER).join("launcher.lock");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // create_new fails when another instance still holds the file.
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => Some(Self { path, _file: file }),
            Err(_) if lock_is_stale(&path) => {
                let _ = std::fs::remove_file(&path);
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .ok()
                    .map(|file| Self { path, _file: file })
            }
            Err(_) => None,
        }
    }
}

/// A lock left behind by a crash should not block the launcher forever.
fn lock_is_stale(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|modified| {
            modified.elapsed().map(|age| age > std::time::Duration::from_secs(8 * 3600)).unwrap_or(false)
        })
        .unwrap_or(false)
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// eframe wrapper that owns the application and routes each frame.
struct Launcher {
    app: GenLauncherApp,
    startup_done: bool,
}

impl Launcher {
    fn new(cc: &eframe::CreationContext<'_>, game: Game) -> Self {
        Self { app: GenLauncherApp::new(cc, game), startup_done: false }
    }
}

impl eframe::App for Launcher {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let _ = frame;
        self.app.pump_messages(ctx);

        match &self.app.phase {
            Phase::Initializing { status } => {
                let status = status.clone();
                ui::dialogs::show_splash(ctx, &self.app, &status);
            }

            Phase::Ready => {
                if !self.startup_done {
                    self.startup_done = true;
                    ui::main_view::apply_game_mode_constraints(&mut self.app);
                }

                ui::main_view::show(&mut self.app, ctx);

                // Modals stack on top, innermost last.
                ui::options_view::show(&mut self.app, ctx);
                ui::dialogs::show_add_mod(&mut self.app, ctx);
                ui::dialogs::show_manual_add(&mut self.app, ctx);
                ui::dialogs::show_dialog(&mut self.app, ctx);
            }
        }

        if self.app.quit_requested() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.app.store.save();
        // Undo every link and rename, so the game folder is left as we found it.
        game::launcher::restore_game_folder();
        game::launcher::delete_temp_folders(config::game_dir());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_icon_decodes_to_a_usable_image() {
        let icon = load_icon().expect("fd.ico must decode");

        assert_eq!(icon.width, icon.height, "the icon should be square");
        // egui wants the dimensions to be a multiple of 4.
        assert_eq!(icon.width % 4, 0, "width {} is not a multiple of 4", icon.width);
        assert_eq!(
            icon.rgba.len(),
            (icon.width * icon.height * 4) as usize,
            "RGBA buffer does not match the stated size"
        );
        // The decoder should pick the largest entry in the file (48px), not the 16px one.
        assert!(icon.width >= 32, "expected the largest entry, got {}px", icon.width);
        assert!(icon.rgba.chunks_exact(4).any(|px| px[3] > 0), "icon is fully transparent");
    }
}

