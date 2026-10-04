//! The main window: tab strip, modification list and the action side panel.

use egui::{Align, Context, Layout, RichText, Vec2};

use crate::app::{Dialog, GenLauncherApp, Tab};
use crate::config::{self, Game};
use crate::game::mod_archive;
use crate::i18n;
use crate::model::{ModificationType, GameModification};
use crate::ui::dialogs::{ManualAddState, AddModState};
use crate::ui::mod_row::{self, RowAction, RowContext};
use crate::ui::theme;
use crate::util::{self, fs as gfs};

pub fn show(app: &mut GenLauncherApp, ctx: &Context) {
    let palette = app.palette;

    // The action column keeps a share of the width rather than a fixed size,
    // so the window can be resized without crowding the mod list.
    let side_width = (ctx.screen_rect().width() * 0.29).clamp(240.0, 340.0);

    egui::SidePanel::right("actions")
        .exact_width(side_width)
        .frame(
            egui::Frame::NONE
                .fill(palette.dark_background)
                .inner_margin(egui::Margin::symmetric(18, 14)),
        )
        .resizable(false)
        .show(ctx, |ui| side_panel(app, ui));

    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(surface(&palette))
                .inner_margin(egui::Margin::symmetric(14, 10)),
        )
        .show(ctx, |ui| {
            tab_strip(app, ui);
            ui.add_space(5.0);
            add_buttons(app, ui);
            ui.add_space(6.0);
            list(app, ui);
        });
}

/// The content surface: a touch lighter than the side column so the two read
/// as separate areas without needing a border.
fn surface(palette: &crate::model::colors::Palette) -> egui::Color32 {
    let base = palette.dark_background;
    egui::Color32::from_rgb(
        base.r().saturating_add(10),
        base.g().saturating_add(10),
        base.b().saturating_add(12),
    )
}

fn tab_strip(app: &mut GenLauncherApp, ui: &mut egui::Ui) {
    let dependency = app
        .store
        .selected_mod_name()
        .unwrap_or_else(|| app.session.game_mode.display_name().to_owned());

    let tabs = [
        (Tab::Mods, i18n::tr("Mods")),
        (Tab::Patches, format!("{}{dependency}", i18n::tr("Patches"))),
        (Tab::Addons, format!("{}{dependency}", i18n::tr("Addons"))),
        (Tab::Exes, i18n::tr("ExesWindow")),
    ];

    // Share the row proportionally so the long "Patches for <mod>" labels
    // never push the last tab out of the panel.
    let spacing = ui.spacing().item_spacing.x;
    let available = (ui.available_width() - spacing * 3.0).max(0.0);
    let weights = [0.14_f32, 0.31, 0.31, 0.24];

    ui.horizontal(|ui| {
        ui.add_enabled_ui(!app.busy, |ui| {
            for ((tab, label), weight) in tabs.into_iter().zip(weights) {
                let selected = app.tab == tab;
                let fill = if selected {
                    app.palette.button_selection
                } else {
                    app.palette.dark_background
                };
                if ui
                    .add_sized(
                        Vec2::new(available * weight, 26.0),
                        egui::Button::new(RichText::new(label).size(13.0)).fill(fill),
                    )
                    .clicked()
                {
                    app.switch_tab(tab);
                }
            }
        });
    });
}

fn add_buttons(app: &mut GenLauncherApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!app.busy, |ui| match app.tab {
            Tab::Mods => {
                if app.store.connected {
                    let blinking = app.store.data.modifications.is_empty();
                    if theme::blinking_button(
                        ui,
                        &app.palette,
                        &i18n::tr("AddMod"),
                        Vec2::new(160.0, 28.0),
                        blinking,
                    )
                    .clicked()
                    {
                        app.add_mod = Some(AddModState {
                            names: app.store.addable_mod_names(),
                            selected: None,
                            filter: String::new(),
                        });
                    }
                }

                if ui
                    .add_sized(
                        Vec2::new(200.0, 28.0),
                        egui::Button::new(RichText::new(i18n::tr("AddModFrFiles")).size(13.0)),
                    )
                    .clicked()
                {
                    start_manual_add(app, ModificationType::Mod);
                }
            }

            Tab::Patches => {
                let label = i18n::trf("AddPatchFromFiles", &[&app.store.dependency_name()]);
                if ui
                    .add_sized(Vec2::new(330.0, 28.0), egui::Button::new(RichText::new(label).size(13.0)))
                    .clicked()
                {
                    start_manual_add(app, ModificationType::Patch);
                }
            }

            Tab::Addons => {
                let label = i18n::trf("AddAddonFromFiles", &[&app.store.dependency_name()]);
                if ui
                    .add_sized(Vec2::new(330.0, 28.0), egui::Button::new(RichText::new(label).size(13.0)))
                    .clicked()
                {
                    start_manual_add(app, ModificationType::Addon);
                }
            }

            Tab::Exes => {}
        });

        if app.busy {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(egui::Spinner::new().size(18.0).color(app.palette.active));
                if !app.busy_status.is_empty() {
                    ui.label(RichText::new(&app.busy_status).size(12.0));
                }
            });
        }
    });
}

fn start_manual_add(app: &mut GenLauncherApp, kind: ModificationType) {
    let Some(files) = crate::ui::dialogs::pick_modification_files() else { return };
    if files.is_empty() {
        return;
    }

    // Stop before asking for a name if an archive cannot be unpacked here.
    // zip and 7z always can; rar needs 7-Zip or UnRAR to be installed.
    if let Some(blocked) = files
        .iter()
        .find(|f| util::archive::is_supported_archive(f) && !util::archive::can_extract(f))
    {
        app.show_dialog(Dialog::error(
            i18n::tr("OperationAborted"),
            i18n::trf("RarToolMissing", &[&gfs::file_name_of(blocked)]),
        ));
        return;
    }

    // An archive's file name usually carries both; the user confirms them.
    let (name, version) = files
        .iter()
        .find(|f| util::archive::is_supported_archive(f))
        .map(|f| mod_archive::guess_name_and_version(&gfs::file_name_of(f)))
        .unwrap_or_default();

    app.manual_add = Some(ManualAddState {
        files,
        kind,
        dependency: app.store.dependency_name(),
        name,
        version,
        error: None,
    });
}

fn list(app: &mut GenLauncherApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match app.tab {
        Tab::Mods => mods_list(app, ui),
        Tab::Patches => dependent_list(app, ui, ModificationType::Patch),
        Tab::Addons => dependent_list(app, ui, ModificationType::Addon),
        Tab::Exes => dependent_list(app, ui, ModificationType::Executable),
    });
}

fn mods_list(app: &mut GenLauncherApp, ui: &mut egui::Ui) {
    let names = app.ordered_mod_names();
    if names.is_empty() {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(i18n::tr("AddMod")).size(18.0).color(app.palette.active));
        });
        return;
    }

    let mut pending: Option<(String, RowAction)> = None;
    let mut drag: Option<(usize, usize)> = None;

    for (index, name) in names.iter().enumerate() {
        let Some(modification) = app.store.modification(ModificationType::Mod, name) else {
            continue;
        };
        let modification = modification.clone();
        let key = crate::net::download::ModKey::new(modification.kind(), name);

        let context = RowContext {
            palette: &app.palette,
            selected: modification.is_selected(),
            download: app.downloads.get(&key),
            installed_versions: app.installed_versions(modification.kind(), name),
            selected_version: modification
                .versions
                .iter()
                .find(|v| v.is_selected && v.installed)
                .map(|v| v.version().to_owned()),
            connected: app.store.connected,
            enabled: !app.busy,
            thank_you: app.thank_you_for.as_ref() == Some(&key),
        };

        let outcome = mod_row::mod_row(ui, &mut app.images, &modification, context);
        let response = &outcome.response;

        // A press that turns into a drag reorders; a plain click selects, and
        // `mod_row` has already turned that into `RowAction::Select`.
        if response.drag_started() {
            egui::DragAndDrop::set_payload(ui.ctx(), index);
        }
        if response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            // Mark the card that is being carried.
            ui.painter().rect_stroke(
                response.rect,
                3,
                egui::Stroke::new(2.0_f32, app.palette.active),
                egui::StrokeKind::Inside,
            );
        }

        // Show where the card would land while one is being carried.
        if let Some(from) = response.dnd_hover_payload::<usize>() {
            if *from != index {
                draw_insertion_marker(ui, response.rect, *from < index, &app.palette);
            }
        }
        if let Some(from) = response.dnd_release_payload::<usize>() {
            drag = Some((*from, index));
        }

        if outcome.action != RowAction::None {
            pending = Some((name.clone(), outcome.action));
        }

        if index + 1 < names.len() {
            card_separator(ui, &app.palette);
        }
    }

    if let Some((from, to)) = drag {
        app.move_mod(from, to);
    }

    if let Some((name, action)) = pending {
        apply_action(app, ui.ctx(), ModificationType::Mod, &name, action);
    }
}

fn dependent_list(app: &mut GenLauncherApp, ui: &mut egui::Ui, kind: ModificationType) {
    let entries: Vec<GameModification> = match kind {
        ModificationType::Patch => {
            app.store.patches_for_selected_mod().into_iter().cloned().collect()
        }
        ModificationType::Addon => {
            app.store.addons_for_selected_mod().into_iter().cloned().collect()
        }
        _ => app.store.exes_for_selected_mod().into_iter().cloned().collect(),
    };

    if entries.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(i18n::tr("ModIsUpToDate"))
                    .size(14.0)
                    .color(app.palette.inactive_border),
            );
        });
        return;
    }

    let mut pending: Option<(String, RowAction)> = None;

    for modification in &entries {
        let name = modification.name().to_owned();
        let key = crate::net::download::ModKey::new(modification.kind(), &name);

        let context = RowContext {
            palette: &app.palette,
            selected: modification.is_selected(),
            download: app.downloads.get(&key),
            installed_versions: app.installed_versions(modification.kind(), &name),
            selected_version: modification
                .versions
                .iter()
                .find(|v| v.is_selected && v.installed)
                .map(|v| v.version().to_owned()),
            connected: app.store.connected,
            enabled: !app.busy,
            thank_you: false,
        };

        let outcome = mod_row::addon_row(ui, modification, context);
        if outcome.action != RowAction::None {
            pending = Some((name, outcome.action));
        }
        card_separator(ui, &app.palette);
    }

    if let Some((name, action)) = pending {
        apply_action(app, ui.ctx(), kind, &name, action);
    }
}

/// A hairline between cards, standing in for the frame the artwork used to draw.
fn card_separator(ui: &mut egui::Ui, palette: &crate::model::colors::Palette) {
    ui.add_space(3.0);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), egui::Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, egui::Stroke::new(1.0_f32, palette.inactive_border.gamma_multiply(0.35)));
    ui.add_space(3.0);
}

/// A line showing the edge the dragged card will be dropped against.
fn draw_insertion_marker(
    ui: &egui::Ui,
    rect: egui::Rect,
    below: bool,
    palette: &crate::model::colors::Palette,
) {
    let y = if below { rect.bottom() } else { rect.top() };
    ui.painter().hline(
        rect.x_range(),
        y,
        egui::Stroke::new(3.0_f32, palette.active),
    );
}

/// Route a row action back into the application state.
fn apply_action(
    app: &mut GenLauncherApp,
    ctx: &Context,
    kind: ModificationType,
    name: &str,
    action: RowAction,
) {
    match action {
        RowAction::None => {}

        RowAction::Select => {
            if kind == ModificationType::Mod {
                app.select_mod(name, ctx);
            } else if kind == ModificationType::Patch {
                select_single_patch(app, name);
            } else {
                toggle_selection(app, kind, name);
            }
        }

        RowAction::Toggle => {
            if kind == ModificationType::Patch {
                select_single_patch(app, name);
            } else {
                toggle_selection(app, kind, name);
            }
        }

        RowAction::Deselect => {
            if kind == ModificationType::Mod {
                app.deselect_mod(ctx);
            } else if let Some(modification) = app.store.modification_mut(kind, name) {
                modification.set_selected(false);
                app.store.save();
            }
        }

        RowAction::Download => {
            app.request_download(crate::net::download::ModKey::new(kind, name));
        }

        RowAction::SelectVersion(version) => {
            app.store.select_version(kind, name, &version);
            app.store.save();
        }

        RowAction::DeleteVersion(version) => {
            app.delete_version(kind, name, &version);
        }

        RowAction::OpenUrl(url) => util::open_external(&url),

        RowAction::OpenModFolder => open_mod_folder(app, kind, name),

        RowAction::SetImage => set_mod_image(app, kind, name, ctx),

        RowAction::OpenGameFolder => {
            util::open_external(&config::game_dir().to_string_lossy());
        }

        RowAction::OpenReplaysFolder => {
            open_user_folder(app, "Replays");
        }

        RowAction::OpenMapsFolder => {
            open_user_folder(app, "Maps");
        }
    }
}

/// Patches behave like radio buttons: picking one clears the rest.
fn select_single_patch(app: &mut GenLauncherApp, name: &str) {
    let already_selected = app
        .store
        .selected_patch()
        .is_some_and(|p| p.name().eq_ignore_ascii_case(name));

    let dependency = app.store.dependency_name();
    for patch in &mut app.store.data.patches {
        if patch.dependence_name().eq_ignore_ascii_case(&dependency) {
            patch.set_selected(false);
        }
    }

    if !already_selected {
        if let Some(patch) = app.store.modification_mut(ModificationType::Patch, name) {
            patch.set_selected(true);
            ensure_a_version_is_picked(patch);
        }
    }
    app.store.save();
}

fn toggle_selection(app: &mut GenLauncherApp, kind: ModificationType, name: &str) {
    if let Some(modification) = app.store.modification_mut(kind, name) {
        let now = !modification.is_selected();
        modification.set_selected(now);
        if now {
            ensure_a_version_is_picked(modification);
        }
    }
    app.store.save();
}

/// Selecting a modification implies selecting one of its versions.
fn ensure_a_version_is_picked(modification: &mut GameModification) {
    if modification.versions.iter().any(|v| v.is_selected && v.installed) {
        return;
    }
    let Some(pick) = modification.display_version().map(|v| v.version().to_owned()) else {
        return;
    };
    for version in &mut modification.versions {
        version.is_selected = version.info.version == pick;
    }
}

fn open_mod_folder(app: &mut GenLauncherApp, kind: ModificationType, name: &str) {
    let Some(modification) = app.store.modification(kind, name) else { return };
    let Some(version) = modification.display_version() else { return };
    let path = version.folder_path();

    if path.is_dir() && gfs::folder_contains_files(&path) {
        util::open_external(&path.to_string_lossy());
        return;
    }

    let (title, message) = match kind {
        ModificationType::Addon => ("UnIsntalledAddon", "NeedToInstallAddon"),
        ModificationType::Patch => ("UnIsntalledPatch", "NeedToInstallPatch"),
        _ => ("UnIsntalledMod", "NeedToInstallMod"),
    };
    app.show_dialog(Dialog::error(i18n::tr(title), i18n::tr(message)));
}

fn open_user_folder(app: &mut GenLauncherApp, sub: &str) {
    let folder = config::user_data_dir(app.session.game_mode, &app.store.data.proton).join(sub);
    if folder.is_dir() {
        util::open_external(&folder.to_string_lossy());
    } else {
        app.show_dialog(Dialog::error(
            i18n::tr("CannotFindPath"),
            folder.to_string_lossy().to_string(),
        ));
    }
}

/// Replace a mod's 500x100 banner with a file the user picks.
fn set_mod_image(
    app: &mut GenLauncherApp,
    kind: ModificationType,
    name: &str,
    ctx: &Context,
) {
    let Some(source) = crate::ui::dialogs::pick_image_file() else { return };
    let Some(modification) = app.store.modification(kind, name) else { return };
    let Some(latest) = modification.latest_version() else { return };

    let folder = config::game_path(config::LAUNCHER_FOLDER)
        .join(config::LAUNCHER_IMAGE_SUBFOLDER)
        .join(gfs::sanitize_file_name(name));
    let target = folder.join(latest.version());

    if let Err(e) = std::fs::create_dir_all(&folder).and_then(|()| {
        let _ = std::fs::remove_file(&target);
        std::fs::copy(&source, &target).map(|_| ())
    }) {
        app.show_dialog(Dialog::error(i18n::tr("OperationAborted"), e.to_string()));
        return;
    }

    app.images.invalidate(&target);
    app.refresh_theme(ctx);
}

// ---------------------------------------------------------------------------
// Side panel
// ---------------------------------------------------------------------------

fn side_panel(app: &mut GenLauncherApp, ui: &mut egui::Ui) {
    let width = ui.available_width();
    let button = Vec2::new(width, 38.0);
    let enabled = !app.busy;

    ui.add_space(10.0);

    ui.add_enabled_ui(enabled, |ui| {
        if ui
            .add_sized(button, egui::Button::new(RichText::new(i18n::tr("Launch")).size(17.0)))
            .clicked()
        {
            app.request_launch(false);
        }

        ui.add_space(4.0);
        if ui
            .add_sized(
                button,
                egui::Button::new(RichText::new(i18n::tr("WorldBuilder")).size(17.0)),
            )
            .clicked()
        {
            app.request_launch(true);
        }

        ui.add_space(4.0);
        let windowed_label =
            if app.store.data.windowed { "ChangeToFullScreen" } else { "ChangeToWindowed" };
        if ui
            .add_sized(button, egui::Button::new(RichText::new(i18n::tr(windowed_label)).size(15.0)))
            .clicked()
        {
            app.store.data.windowed = !app.store.data.windowed;
            app.store.save();
        }

        ui.add_space(4.0);
        // Quick start is a Zero Hour feature only.
        let quick_enabled = app.session.game_mode == Game::ZeroHour;
        let quick_label =
            if app.store.data.quick_start { "ChangeToNormalStart" } else { "ChangeToQuickStart" };
        if ui
            .add_enabled(
                quick_enabled,
                egui::Button::new(RichText::new(i18n::tr(quick_label)).size(15.0))
                    .min_size(button),
            )
            .clicked()
        {
            app.store.data.quick_start = !app.store.data.quick_start;
            app.store.save();
        }

        ui.add_space(4.0);
        if ui
            .add_sized(button, egui::Button::new(RichText::new(i18n::tr("Options")).size(17.0)))
            .clicked()
        {
            app.options = Some(crate::ui::options_view::OptionsState::open(app));
        }

        ui.add_space(12.0);
        if ui
            .add_sized(button, egui::Button::new(RichText::new(i18n::tr("Exit")).size(17.0)))
            .clicked()
        {
            app.request_quit();
        }
    });

    ui.add_space(18.0);
    ui.separator();
    ui.add_space(8.0);

    ui.label(
        RichText::new(format!("{}{}", i18n::tr("CurrentVersion"), config::VERSION)).size(14.0),
    );

    if !app.store.connected {
        ui.label(RichText::new(i18n::tr("NoConnection")).size(14.0));
    }

    // Shown only while the Vulkan layer is being fetched.
    if app.side.active || !app.side.message.is_empty() {
        ui.add_space(6.0);
        theme::progress_bar(
            ui,
            &app.palette,
            app.side.fraction,
            &app.side.message.clone(),
            app.side.active,
            22.0,
        );
    }
}

/// Settings that only make sense for Zero Hour are forced off for Generals,
/// mirroring what `UpdateUIStatus` and `CheckModdedExe` did on start-up.
pub fn apply_game_mode_constraints(app: &mut GenLauncherApp) {
    if app.session.game_mode == Game::Generals {
        app.store.data.modded_exe = false;
        app.store.data.camera_height = 0;
    }
}
