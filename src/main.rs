//! GenLauncher — mod manager for Command & Conquer: Generals and Zero Hour.
//!
//! A Rust/egui port of the original WPF launcher. It keeps the same on-disk
//! layout (`GLM/`, `.GenLauncherFolder/GenLauncherCfg.yaml`) and the same
//! repository manifests, so an existing installation carries over unchanged.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod config;
mod desktop_entry;
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

    config::set_game_dir(locate_game_dir());
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
            .with_title("GenLauncher")
            // Wayland compositors find the window's icon through the .desktop
            // file named after this id (see `desktop_entry`).
            .with_app_id(desktop_entry::APP_ID),
        ..Default::default()
    };

    if let Some(icon) = load_icon() {
        options.viewport = options.viewport.with_icon(icon);
    }
    #[cfg(not(windows))]
    desktop_entry::install(WINDOW_ICON);

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

    let Some(game) = tasks::detect_game().filter(|_| looks_like_a_game_folder(dir)) else {
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

/// The game folder to manage: an explicit path argument, else the working
/// directory, else the folder the launcher binary sits in. The last matters
/// on Linux, where a file manager does not start a program in its own folder.
/// Failing all of those, a Steam install of the game (Zero Hour first).
fn locate_game_dir() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());

    if let Some(arg) = std::env::args_os().nth(1).map(std::path::PathBuf::from) {
        if arg.is_dir() {
            return cwd.join(arg);
        }
    }
    if looks_like_a_game_folder(&cwd) {
        return cwd;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .filter(|dir| looks_like_a_game_folder(dir))
        .or_else(steam_game_dir)
        .unwrap_or(cwd)
}

#[cfg(not(windows))]
fn steam_game_dir() -> Option<std::path::PathBuf> {
    let mut games: Vec<_> = game::steam::installed_game_dirs()
        .into_iter()
        .filter(|dir| looks_like_a_game_folder(dir))
        .collect();
    let is_zero_hour = |dir: &std::path::Path| {
        util::fs::resolve_case_insensitive(dir, "WindowZH.big".as_ref()).exists()
    };
    games.sort_by_key(|dir| !is_zero_hour(dir));
    let dir = games.into_iter().next()?;
    log::info!("using the Steam install in {}", dir.display());
    Some(dir)
}

#[cfg(windows)]
fn steam_game_dir() -> Option<std::path::PathBuf> {
    None
}

/// A real install has the executable, the Bink codec and a window archive.
fn looks_like_a_game_folder(dir: &std::path::Path) -> bool {
    // Case-insensitive, since a copy on Linux may say `Generals.exe`.
    let has = |name: &str| util::fs::resolve_case_insensitive(dir, name.as_ref()).exists();
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
    // Also to the log: on Linux the dialog needs zenity or kdialog to show.
    log::error!("{message}");
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(message)
        .set_level(rfd::MessageLevel::Warning)
        .show();
}

/// The launcher's icon: `assets/icon.png` scaled to 256px. Used for the title
/// bar, the taskbar button and Alt+Tab.
const WINDOW_ICON: &[u8] = include_bytes!("../assets/icon-256.png");

/// Decode the icon. Returns `None` rather than a blank icon, so a failure
/// leaves the platform default in place instead of an empty square.
fn load_icon() -> Option<egui::IconData> {
    let image = image::load_from_memory_with_format(WINDOW_ICON, image::ImageFormat::Png)
        .inspect_err(|e| log::warn!("could not decode the window icon: {e}"))
        .ok()?;

    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(egui::IconData { rgba: rgba.into_raw(), width, height })
}

/// An OS lock on a file in the game folder, so a second launcher cannot start
/// there. The OS drops the lock when the process ends, however it ends, so a
/// crashed or killed launcher never blocks the next start. The file itself
/// stays behind; only the lock on it matters.
struct SingleInstance {
    _file: Option<std::fs::File>,
}

impl SingleInstance {
    /// `None` when another launcher holds the lock.
    fn acquire() -> Option<Self> {
        Self::acquire_at(&config::game_path(config::LAUNCHER_FOLDER).join("launcher.lock"))
    }

    fn acquire_at(path: &std::path::Path) -> Option<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = match std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path) {
            Ok(file) => file,
            Err(e) => {
                // A lock we cannot even open must not stop the launcher.
                log::warn!("cannot open {}: {e}; not guarding against a second launcher", path.display());
                return Some(Self { _file: None });
            }
        };
        match file.try_lock() {
            Ok(()) => Some(Self { _file: Some(file) }),
            Err(std::fs::TryLockError::WouldBlock) => None,
            Err(std::fs::TryLockError::Error(e)) => {
                log::warn!("cannot lock {}: {e}; not guarding against a second launcher", path.display());
                Some(Self { _file: None })
            }
        }
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

        // Closing the window while a game runs would unlink its mod files.
        if ctx.input(|i| i.viewport().close_requested()) && self.app.launch_in_progress() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.app.request_quit();
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
        let icon = load_icon().expect("the icon must decode");

        assert_eq!(icon.width, icon.height, "the icon should be square");
        // egui wants the dimensions to be a multiple of 4.
        assert_eq!(icon.width % 4, 0, "width {} is not a multiple of 4", icon.width);
        assert_eq!(
            icon.rgba.len(),
            (icon.width * icon.height * 4) as usize,
            "RGBA buffer does not match the stated size"
        );
        assert!(icon.width >= 128, "expected the 256px icon, got {}px", icon.width);
        assert!(icon.rgba.as_chunks::<4>().0.iter().any(|px| px[3] > 0), "icon is fully transparent");
    }

    #[test]
    fn only_one_launcher_holds_the_lock_and_it_frees_on_exit() {
        let path = std::env::temp_dir().join(format!("gl-lock-{}/launcher.lock", std::process::id()));
        let first = SingleInstance::acquire_at(&path).expect("the first launcher gets the lock");
        assert!(SingleInstance::acquire_at(&path).is_none(), "a second launcher must be refused");

        // Closing the file is what the OS does for a process that dies.
        drop(first);
        assert!(SingleInstance::acquire_at(&path).is_some(), "the lock must be free again");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

