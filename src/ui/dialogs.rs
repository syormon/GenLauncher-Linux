//! Modal windows: confirmations, "add mod", and manual installation from files.

use egui::{Color32, Context, RichText};

use crate::app::{Answer, DialogKind, GenLauncherApp};
use crate::config;
use crate::i18n;
use crate::model::ModificationType;
use crate::tasks;

/// State of the "select mod to add" window.
pub struct AddModState {
    pub names: Vec<String>,
    pub selected: Option<String>,
    pub filter: String,
}

/// State of the "input modification data" window shown after picking files.
pub struct ManualAddState {
    pub files: Vec<std::path::PathBuf>,
    pub kind: ModificationType,
    /// Mod the patch or addon attaches to; empty for a mod.
    pub dependency: String,
    pub name: String,
    pub version: String,
    pub error: Option<String>,
}

impl ManualAddState {
    /// Where the files end up, relative to the game folder.
    pub fn target_relative(&self) -> String {
        let root = config::MODS_FOLDER;
        match self.kind {
            ModificationType::Patch => format!(
                "{root}/{}/{}/{}/{}",
                self.dependency,
                config::PATCHES_FOLDER_NAME,
                self.name,
                self.version
            ),
            ModificationType::Addon => format!(
                "{root}/{}/{}/{}/{}",
                self.dependency,
                config::ADDONS_FOLDER_NAME,
                self.name,
                self.version
            ),
            _ => format!("{root}/{}/{}", self.name, self.version),
        }
    }

    /// Reject names that cannot become folders, as `CleanInput` did.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err(i18n::tr("EnterModName"));
        }
        if self.version.trim().is_empty() {
            return Err(i18n::tr("EnterModVersion"));
        }
        if !self.version.chars().any(|c| c.is_ascii_digit()) {
            return Err(i18n::tr("VersionMustContainNumbers"));
        }
        if clean(&self.name).is_empty() || clean(&self.version).is_empty() {
            return Err(i18n::tr("NameAndVersionValidSymbols"));
        }
        Ok(())
    }
}

/// Keep only word characters, dot, at and dash, matching the C# regex.
fn clean(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '@' | '-'))
        .collect()
}

/// Draw the active confirmation dialog, if any.
pub fn show_dialog(app: &mut GenLauncherApp, ctx: &Context) {
    let Some(dialog) = &app.dialog else { return };

    let accent = match dialog.kind {
        DialogKind::Error => Color32::from_rgb(220, 70, 60),
        DialogKind::Warning => Color32::from_rgb(230, 170, 40),
        DialogKind::Info => app.palette.active,
    };

    let title = dialog.title.clone();
    let message = dialog.message.clone();
    let confirm_label = dialog.confirm_label.clone();
    let cancel_label = dialog.cancel_label.clone();
    let alternate_label = dialog.alternate_label.clone();
    let has_choice = dialog.has_choice;

    let mut answer = None;

    egui::Modal::new(egui::Id::new("genlauncher-dialog")).show(ctx, |ui| {
        ui.set_width(460.0);

        ui.horizontal(|ui| {
            let glyph = match dialog.kind {
                DialogKind::Error => "\u{26A0}",
                DialogKind::Warning => "\u{26A0}",
                DialogKind::Info => "\u{2139}",
            };
            ui.label(RichText::new(glyph).size(24.0).color(accent));
            ui.label(RichText::new(&title).size(17.0).strong().color(accent));
        });

        ui.add_space(8.0);
        ui.label(RichText::new(&message).size(14.0));
        ui.add_space(14.0);

        // Wrapped, so three long labels still fit the dialog.
        ui.horizontal_wrapped(|ui| {
            if has_choice {
                if ui.button(RichText::new(&confirm_label).size(14.0)).clicked() {
                    answer = Some(Answer::Confirm);
                }
                if let Some(label) = &alternate_label {
                    if ui.button(RichText::new(label).size(14.0)).clicked() {
                        answer = Some(Answer::Alternate);
                    }
                }
                if ui.button(RichText::new(&cancel_label).size(14.0)).clicked() {
                    answer = Some(Answer::Cancel);
                }
            } else if ui.button(RichText::new(i18n::tr("Ok")).size(14.0)).clicked() {
                answer = Some(Answer::Confirm);
            }
        });
    });

    if let Some(answer) = answer {
        app.answer_dialog(answer);
    }
}

/// Draw the "select mod to add" window.
pub fn show_add_mod(app: &mut GenLauncherApp, ctx: &Context) {
    if app.add_mod.is_none() {
        return;
    }

    let mut close = false;
    let mut chosen = None;

    // Taken out so the closure can borrow the rest of the app freely.
    let mut state = app.add_mod.take().expect("checked above");

    egui::Modal::new(egui::Id::new("add-mod")).show(ctx, |ui| {
        ui.set_width(430.0);
        ui.label(RichText::new(i18n::tr("SelectMod")).size(17.0).strong());
        ui.add_space(6.0);

        ui.add(
            egui::TextEdit::singleline(&mut state.filter)
                .hint_text(i18n::tr("ModificationName"))
                .desired_width(f32::INFINITY),
        );
        ui.add_space(6.0);

        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            let filter = state.filter.to_lowercase();
            for name in &state.names {
                if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                    continue;
                }
                let selected = state.selected.as_deref() == Some(name.as_str());
                if ui.selectable_label(selected, RichText::new(name).size(14.0)).clicked() {
                    state.selected = Some(name.clone());
                }
            }
        });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let can_add = state.selected.is_some();
            if ui
                .add_enabled(can_add, egui::Button::new(RichText::new(i18n::tr("Add")).size(14.0)))
                .clicked()
            {
                chosen = state.selected.clone();
                close = true;
            }
            if ui.button(RichText::new(i18n::tr("Cancel")).size(14.0)).clicked() {
                close = true;
            }
        });
    });

    if !close {
        app.add_mod = Some(state);
    }

    if let Some(name) = chosen {
        app.busy = true;
        tasks::spawn_add_mod(
            app.runtime(),
            tasks::StoreSnapshot::of(&app.store),
            name,
            app.tx.clone(),
        );
    }
}

/// Draw the "input modification data" window.
pub fn show_manual_add(app: &mut GenLauncherApp, ctx: &Context) {
    if app.manual_add.is_none() {
        return;
    }

    let mut close = false;
    let mut submit = false;
    let mut state = app.manual_add.take().expect("checked above");

    egui::Modal::new(egui::Id::new("manual-add")).show(ctx, |ui| {
        ui.set_width(430.0);
        ui.label(RichText::new(i18n::tr("InputModData")).size(17.0).strong());
        ui.add_space(8.0);

        ui.label(RichText::new(format!("{} file(s) selected", state.files.len())).size(12.0));
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label(i18n::tr("ModificationName"));
            ui.add(egui::TextEdit::singleline(&mut state.name).desired_width(240.0));
        });
        ui.horizontal(|ui| {
            ui.label(i18n::tr("CurrentVersion"));
            ui.add(egui::TextEdit::singleline(&mut state.version).desired_width(240.0));
        });

        if let Some(error) = &state.error {
            ui.add_space(6.0);
            ui.label(RichText::new(error).color(Color32::from_rgb(220, 70, 60)));
        }

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new(i18n::tr("Add")).size(14.0)).clicked() {
                match state.validate() {
                    Ok(()) => {
                        submit = true;
                        close = true;
                    }
                    Err(message) => state.error = Some(message),
                }
            }
            if ui.button(RichText::new(i18n::tr("Cancel")).size(14.0)).clicked() {
                close = true;
            }
        });
    });

    if submit {
        app.busy = true;
        // Shown at once, before the worker has anything to report.
        app.add_progress = Some(crate::app::AddProgress {
            label: i18n::tr("Preparing"),
            fraction: None,
        });
        tasks::spawn_manual_add(
            app.runtime(),
            state.files.clone(),
            state.target_relative(),
            app.session.game_mode,
            app.tx.clone(),
        );
    }

    if !close {
        app.manual_add = Some(state);
    }
}

/// Splash screen shown while start-up runs.
pub fn show_splash(ui: &mut egui::Ui, app: &GenLauncherApp, status: &str) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(app.palette.dark_background))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.35);
                ui.label(
                    RichText::new("GenLauncher").size(34.0).strong().color(app.palette.active),
                );
                ui.add_space(12.0);
                ui.label(RichText::new(status).size(14.0));
                ui.add_space(16.0);
                ui.add(egui::Spinner::new().size(28.0).color(app.palette.active));
            });
        });

    // Keep animating while we wait.
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
}

/// Open a native file picker for archives the user wants to install by hand.
pub fn pick_modification_files() -> Option<Vec<std::path::PathBuf>> {
    rfd::FileDialog::new()
        .add_filter("Archives and big files", &["big", "7z", "zip", "rar"])
        .set_directory(config::game_dir())
        .pick_files()
}

/// Open a native file picker for a 500x100 mod banner.
pub fn pick_image_file() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter(format!("{} 500x100", i18n::tr("Image")), &["png", "jpg", "jpeg"])
        .pick_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(kind: ModificationType) -> ManualAddState {
        ManualAddState {
            files: vec![],
            kind,
            dependency: "ROTR".into(),
            name: "HUD".into(),
            version: "1.0".into(),
            error: None,
        }
    }

    #[test]
    fn manual_add_targets_match_the_folder_layout() {
        assert_eq!(state(ModificationType::Addon).target_relative(), "GLM/ROTR/Addons/HUD/1.0");
        assert_eq!(state(ModificationType::Patch).target_relative(), "GLM/ROTR/Patches/HUD/1.0");
        assert_eq!(state(ModificationType::Mod).target_relative(), "GLM/HUD/1.0");
    }

    #[test]
    fn manual_add_rejects_bad_names_and_versions() {
        // Messages come from the global catalogue; pin it for the assertions.
        let _guard = i18n::lock_language();
        i18n::set_language("en");

        let mut s = state(ModificationType::Mod);
        assert!(s.validate().is_ok());

        s.version = "beta".into();
        assert_eq!(s.validate().unwrap_err(), i18n::tr("VersionMustContainNumbers"));

        s.version = "1.0".into();
        s.name = "   ".into();
        assert_eq!(s.validate().unwrap_err(), i18n::tr("EnterModName"));

        s.name = "///".into();
        assert_eq!(s.validate().unwrap_err(), i18n::tr("NameAndVersionValidSymbols"));
    }
}
