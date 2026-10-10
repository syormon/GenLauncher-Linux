//! The options screen: launcher settings on the left, the game's own graphics
//! options on the right. Port of `OptionsWindow`.

use egui::{Context, RichText};

use crate::app::GenLauncherApp;
use crate::config::{self, Game};
use crate::game::options::{self, GameOptions, RESOLUTIONS, TOGGLE_KEYS};
use crate::game::{gentool, proton};
use crate::i18n;
use crate::model::ProtonSettings;
use crate::util;

pub struct OptionsState {
    options: Option<GameOptions>,
    /// Shown when `Options.ini` could not be read.
    load_error: Option<String>,
    resolution: String,
    particles: i32,
    texture_quality: i32,
    camera_height: i32,
    use_custom_camera: bool,
    game_params: String,
    force_english: bool,
    /// Edited copy of the Proton settings, saved on Apply.
    proton: ProtonSettings,
    /// What a launch would run with, or why it cannot.
    proton_status: Result<String, String>,
}

impl OptionsState {
    pub fn open(app: &GenLauncherApp) -> Self {
        let proton = app.store.data.proton.clone();
        let mut state = Self {
            options: None,
            load_error: None,
            resolution: String::new(),
            particles: 2500,
            texture_quality: 1,
            camera_height: app.store.data.camera_height,
            use_custom_camera: app.store.data.camera_height != 0,
            game_params: app.store.data.game_params.clone(),
            force_english: english_marker().is_file(),
            proton_status: proton_status(&proton),
            proton,
        };
        state.load_game_options(app.session.game_mode);
        state
    }

    /// Read `Options.ini`, which under Proton lives in the prefix's Documents.
    fn load_game_options(&mut self, game: Game) {
        let (options, load_error) = match GameOptions::load(game, &self.proton) {
            Ok(o) => (Some(o), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };

        self.resolution = options.as_ref().map(|o| o.resolution()).unwrap_or_default();
        self.particles = options.as_ref().map(|o| o.int("MaxParticleCount", 2500)).unwrap_or(2500);
        self.texture_quality = options
            .as_ref()
            .map(|o| options::invert_texture_reduction(o.int("TextureReduction", 1)))
            .unwrap_or(1);
        self.options = options;
        self.load_error = load_error;
    }

    /// A different Proton may mean a different prefix, and `Options.ini`.
    fn proton_changed(&mut self, game: Game) {
        self.proton_status = proton_status(&self.proton);
        self.load_game_options(game);
    }
}

fn proton_status(settings: &ProtonSettings) -> Result<String, String> {
    if cfg!(windows) {
        return Ok(String::new());
    }
    proton::describe(settings).map_err(|e| format!("{e:#}"))
}

/// A file named `eng` in the launcher folder pins the UI to English.
fn english_marker() -> std::path::PathBuf {
    config::game_path(config::LAUNCHER_FOLDER).join("eng")
}

/// Draw the options window. Returns `true` when it should stay open.
pub fn show(app: &mut GenLauncherApp, ctx: &Context) -> bool {
    let Some(mut state) = app.options.take() else { return false };
    let mut keep_open = true;

    egui::Modal::new(egui::Id::new("options")).show(ctx, |ui| {
        ui.set_width(880.0);
        ui.set_height(560.0);

        ui.horizontal(|ui| {
            ui.label(RichText::new(i18n::tr("Options")).size(20.0).strong().color(app.palette.active));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(RichText::new(i18n::tr("Apply")).size(14.0)).clicked() {
                    apply(app, &mut state);
                    keep_open = false;
                }
                if ui.button(RichText::new(i18n::tr("SetDefaultOptions")).size(14.0)).clicked() {
                    app.apply_default_options();
                    state = OptionsState::open(app);
                }
            });
        });

        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.columns(2, |columns| {
                launcher_settings(app, &mut state, &mut columns[0]);
                game_settings(app, &mut state, &mut columns[1]);
            });
        });
    });

    if keep_open {
        app.options = Some(state);
    }
    keep_open
}

fn launcher_settings(app: &mut GenLauncherApp, state: &mut OptionsState, ui: &mut egui::Ui) {
    let palette = app.palette;
    crate::ui::theme::heading(ui, &palette, "GenLauncher");

    let zero_hour = app.session.game_mode == Game::ZeroHour;

    // -- executable --------------------------------------------------------
    ui.add_enabled_ui(zero_hour, |ui| {
        let mut modded = app.store.data.modded_exe;
        if ui.radio_value(&mut modded, true, i18n::tr("ModdedExe")).clicked() {
            app.store.data.modded_exe = true;
        }
        if ui.radio_value(&mut modded, false, i18n::tr("OriginalExe")).clicked() {
            app.store.data.modded_exe = false;
        }
    });

    ui.add_space(6.0);

    // -- GenTool -----------------------------------------------------------
    let mut gentool_on = app.store.data.auto_update_gentool;
    if ui.radio_value(&mut gentool_on, true, i18n::tr("InstallGentool")).clicked() {
        app.store.data.auto_update_gentool = true;
    }
    if ui.radio_value(&mut gentool_on, false, i18n::tr("DisableGentool")).clicked() {
        app.store.data.auto_update_gentool = false;
        gentool::shadow_dll();
    }

    ui.add_space(6.0);

    // -- camera height -----------------------------------------------------
    crate::ui::theme::heading(ui, &palette, "Camera");
    ui.add_enabled_ui(zero_hour, |ui| {
        if ui
            .radio_value(&mut state.use_custom_camera, false, i18n::tr("DefaultCamera"))
            .clicked()
        {
            state.camera_height = 0;
            app.store.data.camera_height = 0;
        }
        if ui
            .radio_value(&mut state.use_custom_camera, true, i18n::tr("CustomCamera"))
            .clicked()
            && state.camera_height == 0
        {
            state.camera_height = 310;
            app.store.data.camera_height = 310;
        }

        if state.use_custom_camera
            && crate::ui::theme::slider(ui, &palette, &mut state.camera_height, 310..=1200)
                .changed()
        {
            app.store.data.camera_height = state.camera_height;
        }
    });

    ui.add_space(6.0);
    crate::ui::theme::heading(ui, &palette, "Mod files");

    let mut check = app.store.data.check_mod_files;
    if ui.checkbox(&mut check, i18n::tr("CheckIntegretyOption")).changed() {
        app.store.data.check_mod_files = check;
    }
    if check {
        let mut ask = app.store.data.ask_before_check;
        if ui.checkbox(&mut ask, i18n::tr("AskToCheck")).changed() {
            app.store.data.ask_before_check = ask;
        }
        ui.label(
            RichText::new(i18n::tr("WarningOption")).size(11.0).color(palette.inactive_border),
        );
    }

    ui.add_space(6.0);

    let mut hide = app.store.data.hide_launcher_after_game_start;
    if ui.checkbox(&mut hide, i18n::tr("HideLauncher")).changed() {
        app.store.data.hide_launcher_after_game_start = hide;
    }

    let mut auto_delete = app.store.data.auto_delete_old_versions;
    if ui.checkbox(&mut auto_delete, i18n::tr("AutoDelete")).changed() {
        app.store.data.auto_delete_old_versions = auto_delete;
    }

    let mut vulkan = app.store.data.use_vulkan;
    if ui.checkbox(&mut vulkan, i18n::tr("Vulkan")).changed() {
        app.store.data.use_vulkan = vulkan;
        // Vulkan and the heat shimmer do not get along.
        if vulkan {
            if let Some(options) = &mut state.options {
                options.set_bool("HeatEffects", false);
            }
        }
    }
    if vulkan {
        ui.label(
            RichText::new(i18n::tr("VulkanWarning")).size(11.0).color(palette.inactive_border),
        );
    }

    if cfg!(not(windows)) {
        ui.add_space(6.0);
        proton_settings(app, state, ui);
    }

    ui.add_space(8.0);
    ui.label(RichText::new(i18n::tr("AdditionalParams")).size(13.0));
    ui.add(egui::TextEdit::singleline(&mut state.game_params).desired_width(f32::INFINITY));

    ui.add_space(10.0);
    crate::ui::theme::heading(ui, &palette, "Language");
    let mut english = state.force_english;
    if ui.radio_value(&mut english, true, "English").clicked() {
        state.force_english = true;
        i18n::set_language("en");
        let _ = std::fs::write(english_marker(), b"");
    }
    if ui.radio_value(&mut english, false, "System language").clicked() {
        state.force_english = false;
        i18n::set_language(i18n::system_language());
        let _ = std::fs::remove_file(english_marker());
    }

    ui.add_space(12.0);
    ui.horizontal_wrapped(|ui| {
        for (label, url) in [
            (i18n::tr("Discord"), config::GENLAUNCHER_DISCORD),
            (
                i18n::tr("Authors"),
                "https://github.com/p0ls3r/GenLauncherModsData/blob/master/Authors.txt",
            ),
            (
                i18n::tr("Sponsors"),
                "https://github.com/p0ls3r/GenLauncherModsData/blob/master/Sponsors.txt",
            ),
            (i18n::tr("Donate"), "https://boosty.to/genlauncher"),
        ] {
            if ui.link(RichText::new(label).size(12.0)).clicked() {
                util::open_external(url);
            }
        }
    });
}

/// The game is a Windows program; off Windows it runs through Steam's Proton.
fn proton_settings(app: &GenLauncherApp, state: &mut OptionsState, ui: &mut egui::Ui) {
    let palette = app.palette;
    crate::ui::theme::heading(ui, &palette, "Proton");
    let mut changed = false;

    ui.label(RichText::new(i18n::tr("ProtonBuild")).size(13.0));
    ui.horizontal(|ui| {
        let width = ui.available_width() - 80.0;
        changed |= ui
            .add(
                egui::TextEdit::singleline(&mut state.proton.proton)
                    .hint_text(i18n::tr("ProtonBuildHint"))
                    .desired_width(width),
            )
            .lost_focus();
        if ui.button(i18n::tr("Browse")).clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                state.proton.proton = path.display().to_string();
                changed = true;
            }
        }
    });

    ui.label(RichText::new(i18n::tr("ProtonEnv")).size(13.0));
    changed |= ui
        .add(
            egui::TextEdit::singleline(&mut state.proton.env)
                .hint_text("DXVK_HUD=fps PROTON_LOG=1")
                .desired_width(f32::INFINITY),
        )
        .lost_focus();

    changed |= ui.checkbox(&mut state.proton.virtual_desktop, i18n::tr("ProtonVirtualDesktop")).changed();

    if changed {
        state.proton_changed(app.session.game_mode);
    }

    match &state.proton_status {
        Ok(text) => ui.label(RichText::new(text).size(11.0).color(palette.inactive_border)),
        Err(error) => {
            ui.label(RichText::new(error).size(11.0).color(egui::Color32::from_rgb(220, 70, 60)))
        }
    };

    if ui.button(i18n::tr("ProtonConfigure")).clicked() {
        if let Err(e) = proton::open_winecfg(&state.proton) {
            state.proton_status = Err(format!("{e:#}"));
        }
    }
}

fn game_settings(app: &mut GenLauncherApp, state: &mut OptionsState, ui: &mut egui::Ui) {
    let palette = app.palette;
    crate::ui::theme::heading(ui, &palette, app.session.game_mode.display_name());

    if let Some(error) = &state.load_error {
        ui.label(RichText::new(error).color(egui::Color32::from_rgb(220, 70, 60)));
        return;
    }
    let Some(options) = &mut state.options else { return };

    // -- resolution --------------------------------------------------------
    ui.horizontal(|ui| {
        ui.label(i18n::tr("Resolution"));
        egui::ComboBox::from_id_salt("resolution")
            .selected_text(&state.resolution)
            .width(160.0)
            .show_ui(ui, |ui| {
                // Keep whatever the file already had, even if it is unusual.
                let mut all: Vec<String> = RESOLUTIONS.iter().map(|s| (*s).to_owned()).collect();
                if !state.resolution.is_empty() && !all.contains(&state.resolution) {
                    all.push(state.resolution.clone());
                }
                for entry in all {
                    if crate::ui::theme::selectable_row(ui, entry == state.resolution, entry.as_str())
                        .clicked()
                    {
                        state.resolution = entry.clone();
                        options.set_resolution(&entry);
                    }
                }
            });
    });

    ui.add_space(6.0);
    ui.label(i18n::tr("MaximumPart"));
    if crate::ui::theme::slider(ui, &palette, &mut state.particles, 100..=10000).changed() {
        options.set("MaxParticleCount", &state.particles.to_string());
    }

    ui.add_space(6.0);
    ui.label(i18n::tr("Texture"));
    if crate::ui::theme::slider(ui, &palette, &mut state.texture_quality, 0..=2).changed() {
        options.set(
            "TextureReduction",
            &options::invert_texture_reduction(state.texture_quality).to_string(),
        );
    }
    ui.label(RichText::new(i18n::tr("LowAndHigh")).size(11.0).color(palette.inactive_border));

    ui.add_space(10.0);
    crate::ui::theme::heading(ui, &palette, i18n::tr("ToggleOff").as_str());

    for (key, label_key) in TOGGLE_KEYS {
        let mut value = options.is_yes(key);
        if ui.checkbox(&mut value, i18n::tr(label_key)).changed() {
            options.set_bool(key, value);
        }
    }

    // Stored inverted: the UI offers "disable", the file stores "DynamicLOD".
    let mut disable_lod = !options.is_yes("DynamicLOD");
    if ui.checkbox(&mut disable_lod, i18n::tr("DisableLOD")).changed() {
        options.set_bool("DynamicLOD", !disable_lod);
    }
}

fn apply(app: &mut GenLauncherApp, state: &mut OptionsState) {
    if let Some(options) = &state.options {
        if let Err(e) = options.save() {
            log::warn!("could not save Options.ini: {e:#}");
        }
    }

    app.store.data.game_params = state.game_params.clone();
    app.store.data.proton = state.proton.clone();
    app.store.data.camera_height = if state.use_custom_camera { state.camera_height } else { 0 };
    app.store.save();
}
