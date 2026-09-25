//! One row in the mods / patches / addons / executables list.
//!
//! Mods get the tall layout with 500x100 artwork; everything else uses the
//! compact one, matching `ModificationTemplate` and `AddonsTemplate`.

use egui::{Align, Layout, RichText, Sense, Ui, Vec2};

use crate::app::DownloadState;
use crate::config;
use crate::i18n;
use crate::model::colors::Palette;
use crate::model::{GameModification, ModificationType};
use crate::ui::theme;
use crate::util::fs as gfs;

/// What the user did on a row this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowAction {
    None,
    /// Left click: make this the selection (single-select lists).
    Select,
    /// Left click in a multi-select list.
    Toggle,
    /// Right click: drop it out of the selection.
    Deselect,
    Download,
    SelectVersion(String),
    DeleteVersion(String),
    OpenUrl(String),
    OpenModFolder,
    SetImage,
    OpenGameFolder,
    OpenReplaysFolder,
    OpenMapsFolder,
}

/// A drawn row: what the user did, plus the card's own background response so
/// the list can drive drag-and-drop with it.
pub struct RowOutcome {
    pub action: RowAction,
    pub response: egui::Response,
}

pub struct RowContext<'a> {
    pub palette: &'a Palette,
    pub selected: bool,
    pub download: Option<&'a DownloadState>,
    pub installed_versions: Vec<String>,
    pub selected_version: Option<String>,
    pub connected: bool,
    pub enabled: bool,
    /// Pulse the support button after a good session.
    pub thank_you: bool,
}

/// Draw a full mod row.
///
/// The card senses clicks and drags on its *background*, which egui registers
/// below the widgets inside it — so the buttons and the version drop-down keep
/// priority, and a click anywhere else on the card selects the mod.
pub fn mod_row(
    ui: &mut Ui,
    app_images: &mut crate::ui::images::ImageCache,
    modification: &GameModification,
    ctx: RowContext<'_>,
) -> RowOutcome {
    let mut action = RowAction::None;
    let palette = ctx.palette;

    let frame = egui::Frame::NONE
        .fill(if ctx.selected {
            palette.list_selection1
        } else {
            egui::Color32::TRANSPARENT
        })
        .inner_margin(egui::Margin::symmetric(10, 6))
        .corner_radius(3);

    let card = ui.scope_builder(
        egui::UiBuilder::new().sense(Sense::click_and_drag()),
        |ui| {
        frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            // -- header: name, latest version, version picker ---------------
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(name_text(modification, ctx.selected, palette).size(15.0));
                    ui.label(
                        RichText::new(latest_version_label(modification))
                            .size(14.0)
                            .color(if ctx.selected {
                                palette.default_text
                            } else {
                                palette.inactive_border
                            }),
                    );
                });

                if ctx.selected {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(picked) = version_picker(ui, modification, &ctx) {
                            action = picked;
                        }
                    });
                }
            });

            // -- artwork ----------------------------------------------------
            if let Some(texture) =
                artwork(ui.ctx(), app_images, modification, ctx.selected, &ctx)
            {
                ui.add_space(3.0);
                // Fit the banner to the panel, capped so tall lists stay usable.
                let width = ui
                    .available_width()
                    .min(theme::MOD_IMAGE_MAX_HEIGHT * theme::MOD_IMAGE_ASPECT);
                let size = Vec2::new(width, width / theme::MOD_IMAGE_ASPECT);
                ui.vertical_centered(|ui| {
                    let border =
                        if ctx.selected { palette.active } else { palette.inactive_border };
                    // Stroke the image's own rect so the outline hugs the banner
                    // instead of stretching across the whole card.
                    let image = ui.add(egui::Image::new(&texture).fit_to_exact_size(size));
                    ui.painter().rect_stroke(
                        image.rect,
                        2,
                        egui::Stroke::new(1.0_f32, border),
                        egui::StrokeKind::Outside,
                    );
                });
                ui.add_space(3.0);
            }

            // -- progress ---------------------------------------------------
            let (fraction, message, active) = match ctx.download {
                Some(state) => (state.fraction, state.message.clone(), true),
                None => (0.0, String::new(), false),
            };
            theme::progress_bar(ui, palette, fraction, &message, active, 17.0);

            // -- buttons ----------------------------------------------------
            ui.add_space(3.0);
            ui.horizontal_wrapped(|ui| {
                if let Some(pressed) = update_button(ui, modification, &ctx) {
                    action = pressed;
                }

                let info = &modification.latest.info;

                if ctx.selected && !info.support_link.is_empty() {
                    let label = i18n::tr("Donate");
                    let fill = if ctx.thank_you {
                        theme::blink(ui.ctx(), palette.dark_background, palette.button_selection)
                    } else {
                        palette.dark_background
                    };
                    if ui
                        .add_sized(
                            Vec2::new(96.0, 25.0),
                            egui::Button::new(RichText::new(label).size(13.0)).fill(fill),
                        )
                        .clicked()
                    {
                        action = RowAction::OpenUrl(info.support_link.clone());
                    }
                }

                if ctx.selected && !info.network_info.is_empty() {
                    let label = i18n::tr("PlayOnline");
                    if ui
                        .add_sized(Vec2::new(140.0, 25.0), egui::Button::new(RichText::new(label).size(13.0)))
                        .clicked()
                    {
                        action = RowAction::OpenUrl(info.network_info.clone());
                    }
                }

                if ctx.selected && !info.news_link.is_empty() {
                    let label = i18n::tr("ChangelogOnly");
                    if ui
                        .add_sized(
                            Vec2::new(96.0, 25.0),
                            egui::Button::new(RichText::new(label).size(13.0)),
                        )
                        .clicked()
                    {
                        action = RowAction::OpenUrl(info.news_link.clone());
                    }
                }
            });
        });
        },
    );

    let response = card.response;

    if response.clicked() && action == RowAction::None {
        action = RowAction::Select;
    }
    if response.secondary_clicked() && action == RowAction::None {
        action = RowAction::Deselect;
    }

    if let Some(menu_action) = context_menu(&response, modification) {
        action = menu_action;
    }

    RowOutcome { action, response }
}

/// Compact row used for patches, addons and executables.
pub fn addon_row(
    ui: &mut Ui,
    modification: &GameModification,
    ctx: RowContext<'_>,
) -> RowOutcome {
    let mut action = RowAction::None;
    let palette = ctx.palette;

    let frame = egui::Frame::NONE
        .fill(if ctx.selected {
            palette.list_selection1
        } else {
            egui::Color32::TRANSPARENT
        })
        .inner_margin(egui::Margin::symmetric(10, 4))
        .corner_radius(3);

    let card = ui.scope_builder(
        egui::UiBuilder::new().sense(Sense::click()),
        |ui| {
        frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(name_text(modification, ctx.selected, palette).size(13.0).strong());
                    ui.label(
                        RichText::new(latest_version_label(modification))
                            .size(13.0)
                            .color(if ctx.selected {
                                palette.default_text
                            } else {
                                palette.inactive_border
                            }),
                    );
                });

                if ctx.selected {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(picked) = version_picker(ui, modification, &ctx) {
                            action = picked;
                        }
                    });
                }
            });

            let (fraction, message, active) = match ctx.download {
                Some(state) => (state.fraction, state.message.clone(), true),
                None => (0.0, String::new(), false),
            };
            theme::progress_bar(ui, palette, fraction, &message, active, 16.0);

            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                if let Some(pressed) = update_button(ui, modification, &ctx) {
                    action = pressed;
                }

                let info = &modification.latest.info;
                if ctx.selected && !info.network_info.is_empty()
                    && ui
                        .add_sized(
                            Vec2::new(130.0, 24.0),
                            egui::Button::new(RichText::new(i18n::tr("PlayOnline")).size(12.0)),
                        )
                        .clicked()
                    {
                        action = RowAction::OpenUrl(info.network_info.clone());
                    }

                if ctx.selected && !info.news_link.is_empty()
                    && ui
                        .add_sized(
                            Vec2::new(112.0, 24.0),
                            egui::Button::new(
                                RichText::new(i18n::tr("ChangelogOnly")).size(12.0),
                            ),
                        )
                        .clicked()
                    {
                        action = RowAction::OpenUrl(info.news_link.clone());
                    }
            });
        });
        },
    );

    let response = card.response;

    if response.clicked() && action == RowAction::None {
        action = RowAction::Toggle;
    }
    if response.secondary_clicked() && action == RowAction::None {
        action = RowAction::Deselect;
    }

    if let Some(menu_action) = context_menu(&response, modification) {
        action = menu_action;
    }

    RowOutcome { action, response }
}

fn name_text(modification: &GameModification, selected: bool, palette: &Palette) -> RichText {
    let text = RichText::new(modification.name());
    if selected {
        text.color(palette.active).strong()
    } else {
        text.color(palette.inactive_border)
    }
}

fn latest_version_label(modification: &GameModification) -> String {
    let version = modification
        .latest_version()
        .map(|v| v.version().to_owned())
        .unwrap_or_default();
    format!("{}{version}", i18n::tr("LatestVersion"))
}

/// Drop-down of installed versions, with a Delete button beside each one.
fn version_picker(
    ui: &mut Ui,
    modification: &GameModification,
    ctx: &RowContext<'_>,
) -> Option<RowAction> {
    if ctx.installed_versions.is_empty() {
        return None;
    }

    let mut action = None;
    let current = ctx.selected_version.clone().unwrap_or_else(|| {
        ctx.installed_versions.first().cloned().unwrap_or_default()
    });

    let downloading = ctx.download.is_some();

    ui.add_enabled_ui(ctx.enabled && !downloading, |ui| {
        egui::ComboBox::from_id_salt(("versions", modification.name()))
            .selected_text(RichText::new(&current).size(13.0))
            .width(190.0)
            .show_ui(ui, |ui| {
                for version in &ctx.installed_versions {
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(*version == current, RichText::new(version).size(13.0))
                            .clicked()
                        {
                            action = Some(RowAction::SelectVersion(version.clone()));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button(i18n::tr("Delete")).clicked() {
                                action = Some(RowAction::DeleteVersion(version.clone()));
                            }
                        });
                    });
                }
            });
    });

    action
}

/// The Update / Install / Stop button, with the blink that flags a new version.
fn update_button(
    ui: &mut Ui,
    modification: &GameModification,
    ctx: &RowContext<'_>,
) -> Option<RowAction> {
    let palette = ctx.palette;

    let downloading = ctx.download.is_some();
    let latest_installed =
        modification.latest_version().map(|v| v.installed).unwrap_or(false);
    let nothing_installed = !modification.versions.iter().any(|v| v.installed);
    let can_fetch = modification.versions.iter().any(|v| {
        !v.info.simple_download_link.is_empty() || !v.info.s3_folder_name.is_empty()
    });

    let label = if downloading {
        i18n::tr("Stop")
    } else if nothing_installed {
        i18n::tr("Install")
    } else {
        i18n::tr("Update")
    };

    let enabled = ctx.enabled && (downloading || (!latest_installed && can_fetch && ctx.connected));

    let fill = if !downloading && !latest_installed && can_fetch && ctx.connected {
        theme::blink(ui.ctx(), palette.dark_background, palette.button_selection)
    } else {
        palette.dark_background
    };

    let response = ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).size(15.0))
            .fill(fill)
            .min_size(Vec2::new(128.0, 25.0)),
    );

    response.clicked().then_some(RowAction::Download)
}

/// The per-row right-click menu.
fn context_menu(
    response: &egui::Response,
    modification: &GameModification,
) -> Option<RowAction> {
    let mut action = None;

    response.context_menu(|ui| {
        if ui.button(i18n::tr("OpenModFolder")).clicked() {
            action = Some(RowAction::OpenModFolder);
            ui.close();
        }
        if ui.button(i18n::tr("SetImage")).clicked() {
            action = Some(RowAction::SetImage);
            ui.close();
        }

        let info = &modification.latest.info;
        if !info.mod_db_link.is_empty() && ui.button(i18n::tr("Moddb")).clicked() {
            action = Some(RowAction::OpenUrl(info.mod_db_link.clone()));
            ui.close();
        }
        if !info.discord_link.is_empty() && ui.button(i18n::tr("Discord")).clicked() {
            action = Some(RowAction::OpenUrl(info.discord_link.clone()));
            ui.close();
        }

        ui.separator();
        if ui.button(i18n::tr("OpenGameFolder")).clicked() {
            action = Some(RowAction::OpenGameFolder);
            ui.close();
        }
        if ui.button(i18n::tr("OpenReplaysFolder")).clicked() {
            action = Some(RowAction::OpenReplaysFolder);
            ui.close();
        }
        if ui.button(i18n::tr("OpenMapsFolder")).clicked() {
            action = Some(RowAction::OpenMapsFolder);
            ui.close();
        }
    });

    action
}

/// Pick the artwork for a row: the mod's own banner, or the stock placeholder.
fn artwork(
    ctx: &egui::Context,
    images: &mut crate::ui::images::ImageCache,
    modification: &GameModification,
    selected: bool,
    row: &RowContext<'_>,
) -> Option<egui::TextureHandle> {
    let _ = row;
    match modification.kind() {
        ModificationType::Mod => {
            let latest = modification.latest_version()?;
            let own = config::game_path(config::LAUNCHER_FOLDER)
                .join(config::LAUNCHER_IMAGE_SUBFOLDER)
                .join(gfs::sanitize_file_name(modification.name()))
                .join(latest.version());

            let path = if own.is_file() {
                own
            } else {
                crate::tasks::placeholder_image_path()
            };
            images.get(ctx, &path, !selected)
        }

        _ => None,
    }
}
