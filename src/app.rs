//! The application object: state, the background-message pump, and the
//! multi-step launch flow. The drawing lives in [`crate::ui`].

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::config::{self, Game, SessionInfo};
use crate::game::options::GameOptions;
use crate::game::gentool;
use crate::i18n;
use crate::model::colors::Palette;
use crate::model::{compare_versions, ModVersion, ModificationType};
use crate::net::download::{Cancel, DownloadEvent, ModKey};
use crate::state::Store;
use crate::tasks::{self, Bg};
use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Mods,
    Patches,
    Addons,
    Exes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Info,
    Warning,
    Error,
}

/// What a dialog's confirm button should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogId {
    /// Dismiss only.
    Acknowledge,
    ApplyDefaults { world_builder: bool },
    ConfirmUpdates { world_builder: bool },
    ConfirmDeprecated { world_builder: bool },
    ConfirmIntegrity { world_builder: bool },
    SyncSystemTime { key: ModKey },
    DownloadDeprecated { key: ModKey },
}

pub struct Dialog {
    pub id: DialogId,
    pub kind: DialogKind,
    pub title: String,
    pub message: String,
    pub confirm_label: String,
    pub cancel_label: String,
    /// `false` for a plain acknowledgement with a single OK button.
    pub has_choice: bool,
}

impl Dialog {
    pub fn error(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: DialogId::Acknowledge,
            kind: DialogKind::Error,
            title: title.into(),
            message: message.into(),
            confirm_label: i18n::tr("Ok"),
            cancel_label: i18n::tr("Cancel"),
            has_choice: false,
        }
    }

    pub fn info(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind: DialogKind::Info, ..Self::error(title, message) }
    }

    pub fn confirm(
        id: DialogId,
        title: impl Into<String>,
        message: impl Into<String>,
        confirm_label: impl Into<String>,
    ) -> Self {
        Self {
            id,
            kind: DialogKind::Warning,
            title: title.into(),
            message: message.into(),
            confirm_label: confirm_label.into(),
            cancel_label: i18n::tr("Cancel"),
            has_choice: true,
        }
    }
}

/// Live state of one modification download.
pub struct DownloadState {
    pub cancel: Cancel,
    pub fraction: f32,
    pub message: String,
}

/// The right-hand progress bar. Only the Vulkan layer uses it now.
#[derive(Default)]
pub struct SideBarDownload {
    pub active: bool,
    pub fraction: f32,
    pub message: String,
}

pub enum Phase {
    Initializing { status: String },
    Ready,
}

pub struct GenLauncherApp {
    runtime: tokio::runtime::Runtime,
    pub tx: Sender<Bg>,
    rx: Receiver<Bg>,

    pub phase: Phase,
    pub store: Store,
    pub session: SessionInfo,
    pub palette: Palette,
    pub default_palette: Palette,

    pub tab: Tab,
    pub images: ui::images::ImageCache,

    pub downloads: HashMap<ModKey, DownloadState>,
    pub side: SideBarDownload,

    pub dialog: Option<Dialog>,
    /// Errors queued while a dialog is already up.
    pending_dialogs: Vec<Dialog>,

    pub busy: bool,
    pub game_running: bool,
    pub world_builder_running: bool,

    pub options: Option<ui::options_view::OptionsState>,
    pub add_mod: Option<ui::dialogs::AddModState>,
    pub manual_add: Option<ui::dialogs::ManualAddState>,

    /// Mod whose support button should pulse after a good session.
    pub thank_you_for: Option<ModKey>,

    quit_requested: bool,
}

impl GenLauncherApp {
    pub fn new(cc: &eframe::CreationContext<'_>, game: Game) -> Self {
        let (tx, rx) = channel();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("tokio runtime");

        let palette = Palette::for_game(game);

        let init_tx = tx.clone();
        let ctx = cc.egui_ctx.clone();
        runtime.spawn(async move {
            tasks::initialize(game, init_tx).await;
            ctx.request_repaint();
        });

        ui::theme::apply(&cc.egui_ctx, &palette);

        Self {
            runtime,
            tx,
            rx,
            phase: Phase::Initializing { status: i18n::tr("Preparing") },
            store: Store::new(game),
            session: tasks::session_info(game, false),
            palette,
            default_palette: palette,
            tab: Tab::Mods,
            images: ui::images::ImageCache::default(),
            downloads: HashMap::new(),
            side: SideBarDownload::default(),
            dialog: None,
            pending_dialogs: Vec::new(),
            busy: false,
            game_running: false,
            world_builder_running: false,
            options: None,
            add_mod: None,
            manual_add: None,
            thank_you_for: None,
            quit_requested: false,
        }
    }

    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }

    // -- dialogs ----------------------------------------------------------

    pub fn show_dialog(&mut self, dialog: Dialog) {
        if self.dialog.is_none() {
            self.dialog = Some(dialog);
        } else {
            self.pending_dialogs.push(dialog);
        }
    }

    /// Act on the user's answer to the dialog currently on screen.
    pub fn answer_dialog(&mut self, confirmed: bool) {
        let Some(dialog) = self.dialog.take() else { return };
        self.dialog = self.pending_dialogs.pop();

        match dialog.id {
            DialogId::Acknowledge => {}

            DialogId::ApplyDefaults { world_builder } => {
                if confirmed {
                    self.apply_default_options();
                }
                self.store.data.first_start = false;
                self.advance_launch(world_builder, LaunchStep::CheckUpdates);
            }

            DialogId::ConfirmUpdates { world_builder } => {
                if confirmed {
                    self.advance_launch(world_builder, LaunchStep::CheckDeprecated);
                }
            }

            DialogId::ConfirmDeprecated { world_builder } => {
                if confirmed {
                    self.advance_launch(world_builder, LaunchStep::CheckIntegrity);
                }
            }

            DialogId::ConfirmIntegrity { world_builder } => {
                self.start_launch(world_builder, confirmed);
            }

            DialogId::SyncSystemTime { key } => {
                if confirmed {
                    // Setting the clock needs privileges we do not ask for;
                    // point at the OS setting instead of failing silently.
                    self.show_dialog(Dialog::error(
                        i18n::tr("CantSync"),
                        i18n::tr("SyncManually"),
                    ));
                }
                self.downloads.remove(&key);
            }

            DialogId::DownloadDeprecated { key } => {
                if confirmed {
                    self.begin_download(&key);
                }
            }
        }
    }

    // -- theme ------------------------------------------------------------

    /// Re-theme the window for the selected mod, or back to the game default.
    pub fn refresh_theme(&mut self, ctx: &egui::Context) {
        let mut palette = self.default_palette;

        // A mod may ship its own palette; the launcher itself draws flat, so
        // only the colours are taken from it.
        if let Some(selected) = self.store.selected_mod() {
            if let Some(colors) = &selected.latest.info.colors_information {
                palette = palette.with_overrides(colors);
            }
        }

        if palette != self.palette {
            self.palette = palette;
            ui::theme::apply(ctx, &palette);
        }
    }

    // -- background messages ----------------------------------------------

    pub fn pump_messages(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.rx.try_recv() {
            self.handle_message(message, ctx);
        }
    }

    fn handle_message(&mut self, message: Bg, ctx: &egui::Context) {
        match message {
            Bg::InitStatus(status) => {
                if let Phase::Initializing { status: slot } = &mut self.phase {
                    *slot = status;
                }
            }

            Bg::InitDone(store) => {
                self.store = *store;
                self.session = tasks::session_info(self.store.game_mode, self.store.connected);
                self.on_ready(ctx);
            }

            Bg::Download(event) => self.handle_download_event(event),

            Bg::ModDetails { mod_name, versions } => {
                self.store.repos.mark_details_loaded(&mod_name);
                for version in versions {
                    self.store.add_repos_version(version);
                }
                self.store.refresh_local_modifications();
                self.busy = false;
            }

            Bg::OriginalGameExtras(versions) => {
                self.store.repos.mark_details_loaded(config::ORIGINAL_GAME_ALIAS);
                for version in versions {
                    self.store.add_repos_version(version);
                }
                self.store.refresh_local_modifications();
                self.busy = false;
            }

            Bg::ModAdded { version, extras } => {
                let name = version.name.clone();
                self.store.add_repos_version(*version);
                for extra in extras {
                    self.store.add_repos_version(extra);
                }
                self.store.repos.mark_details_loaded(&name);

                // A newly added mod goes to the top of the list.
                if let Some(index) = self
                    .store
                    .data
                    .modifications
                    .iter()
                    .position(|m| m.name().eq_ignore_ascii_case(&name))
                {
                    let entry = self.store.data.modifications.remove(index);
                    self.store.data.modifications.insert(0, entry);
                    self.renumber_mods();
                }

                self.store.refresh_local_modifications();
                self.busy = false;
            }

            Bg::ManualAddDone { error } => {
                self.busy = false;
                self.store.refresh_local_modifications();
                self.renumber_mods();
                if let Some(error) = error {
                    self.show_dialog(Dialog::error(i18n::tr("OperationAborted"), error));
                }
            }

            Bg::SideProgress { label, total, read } => {
                let fraction = total
                    .filter(|t| *t > 0)
                    .map(|t| (read as f32 / t as f32).clamp(0.0, 1.0))
                    .unwrap_or(0.0);
                self.side.fraction = fraction;
                self.side.message = match total {
                    Some(total) => i18n::trf(
                        "DownloadInProgress",
                        &[&(read / 1_048_576).to_string(), &(total / 1_048_576).to_string()],
                    ),
                    None => format!("{label}: {} MB", read / 1_048_576),
                };
            }

            Bg::SideDone { error } => {
                self.side.active = false;
                self.side.fraction = 0.0;
                self.side.message = match error {
                    Some(error) => format!("{}{error}", i18n::tr("Error")),
                    None => String::new(),
                };
            }

            Bg::GamePrepared { world_builder, ok } => {
                self.busy = false;
                if !ok {
                    self.game_running = false;
                    self.world_builder_running = false;
                    let _ = world_builder;
                    self.show_dialog(Dialog::error(
                        i18n::tr("FilesCorrupted"),
                        i18n::tr("Reinstall"),
                    ));
                }
            }

            Bg::GameFinished { world_builder, played_long_enough, error } => {
                self.busy = false;
                if world_builder {
                    self.world_builder_running = false;
                } else {
                    self.game_running = false;
                }

                if let Some(error) = error {
                    self.show_dialog(Dialog::error(i18n::tr("LaunchAborted"), error));
                } else if played_long_enough {
                    self.thank_you_for =
                        self.store.selected_mod_name().map(|n| ModKey::new(ModificationType::Mod, &n));
                }

                self.store.refresh_local_modifications();
                self.store.save();
            }

            Bg::Error { title, message } => {
                self.busy = false;
                self.show_dialog(Dialog::error(title, message));
            }
        }
    }

    fn on_ready(&mut self, ctx: &egui::Context) {
        self.phase = Phase::Ready;

        self.sort_mods_by_saved_order();
        self.update_launch_count();
        self.refresh_theme(ctx);

        if !self.store.connected {
            self.show_dialog(Dialog::info(
                i18n::tr("NoConnection"),
                i18n::tr("CannotConnect"),
            ));
        }
    }

    /// Kept only so the count in an existing config keeps ticking over.
    fn update_launch_count(&mut self) {
        self.store.data.launches_count = self.store.data.launches_count.saturating_add(1);
    }

    pub fn renumber_mods(&mut self) {
        for (index, modification) in self.store.data.modifications.iter_mut().enumerate() {
            modification.number_in_list = index as i32;
        }
    }

    // -- downloads --------------------------------------------------------

    fn handle_download_event(&mut self, event: DownloadEvent) {
        match event {
            DownloadEvent::Progress { key, total, read, percent, file } => {
                let Some(state) = self.downloads.get_mut(&key) else { return };
                state.fraction = percent.map(|p| (p / 100.0) as f32).unwrap_or(0.0);

                let megabytes = |b: u64| (b / 1_048_576).to_string();
                state.message = match (percent, total, file) {
                    (Some(p), _, _) if p >= 100.0 => i18n::tr("UnpackingPreparing"),
                    (_, Some(total), Some(file)) => i18n::trf(
                        "Download",
                        &[&file, &megabytes(read), &megabytes(total)],
                    ),
                    (_, Some(total), None) => i18n::trf(
                        "DownloadInProgress",
                        &[&megabytes(read), &megabytes(total)],
                    ),
                    _ => format!("{} MB", megabytes(read)),
                };
            }

            DownloadEvent::Message { key, text } => {
                if let Some(state) = self.downloads.get_mut(&key) {
                    state.message = text;
                }
            }

            DownloadEvent::Done { key, result } => {
                self.downloads.remove(&key);

                if result.canceled {
                    self.store.refresh_local_modifications();
                    return;
                }

                if result.crashed || result.timed_out {
                    self.show_dialog(Dialog::error(
                        i18n::tr("Error"),
                        result.message,
                    ));
                    self.store.refresh_local_modifications();
                    return;
                }

                self.on_download_succeeded(&key);
            }
        }
    }

    fn on_download_succeeded(&mut self, key: &ModKey) {
        if self.store.data.auto_delete_old_versions {
            self.delete_outdated_versions(key);
        }

        self.store.refresh_local_modifications();

        // Select the version that was just installed.
        if let Some(modification) = self.store.modification_mut(key.kind, &key.name) {
            if let Some(newest) =
                modification.versions_ordered().into_iter().rfind(|v| v.installed)
            {
                let version = newest.version().to_owned();
                for v in &mut modification.versions {
                    v.is_selected = v.info.version == version;
                }
                modification.latest.installed = true;
            }
        }

        self.store.save();
    }

    fn delete_outdated_versions(&mut self, key: &ModKey) {
        let Some(modification) = self.store.modification(key.kind, &key.name) else { return };
        let Some(latest) = modification.latest_version().map(|v| v.version().to_owned()) else {
            return;
        };

        let stale: Vec<ModVersion> = modification
            .versions
            .iter()
            .filter(|v| v.version() != latest)
            .cloned()
            .collect();

        for version in stale {
            if let Err(e) = self.store.delete_version(&version) {
                log::warn!("could not delete {} {}: {e:#}", version.name(), version.version());
            }
        }
    }

    /// The Update/Install button was pressed on a row.
    pub fn request_download(&mut self, key: ModKey) {
        log::info!("download requested for {:?} {}", key.kind, key.name);
        if self.downloads.contains_key(&key) {
            // Pressing it again means stop.
            if let Some(state) = self.downloads.get(&key) {
                state.cancel.cancel();
            }
            return;
        }

        let Some(modification) = self.store.modification(key.kind, &key.name) else { return };

        if modification.latest.info.deprecated {
            let message = i18n::trf("Deprecated", &[modification.name()]);
            self.show_dialog(Dialog::confirm(
                DialogId::DownloadDeprecated { key },
                i18n::tr("Compatibility"),
                message,
                i18n::tr("Continue"),
            ));
            return;
        }

        self.begin_download(&key);
    }

    fn begin_download(&mut self, key: &ModKey) {
        let Some(modification) = self.store.modification(key.kind, &key.name) else {
            log::warn!("no modification named {} to download", key.name);
            return;
        };
        let Some(latest) = modification.latest_version().cloned() else {
            log::warn!("{} has no version to download", key.name);
            return;
        };
        log::info!(
            "starting download of {} {} (s3 bucket {:?})",
            latest.name(),
            latest.version(),
            latest.info.s3_bucket_name
        );

        let uses_s3 =
            !latest.info.s3_host_link.is_empty() && !latest.info.s3_folder_name.is_empty();

        // S3 requests are signed, so a skewed clock makes every one fail.
        if uses_s3 && tasks::system_time_is_out_of_sync() {
            log::warn!("system clock looks out of sync; S3 signatures would be rejected");
            self.show_dialog(Dialog::confirm(
                DialogId::SyncSystemTime { key: key.clone() },
                i18n::tr("OutOfSync"),
                i18n::tr("SyncTime"),
                i18n::tr("SyncNow"),
            ));
            return;
        }

        let previous_folder = modification
            .latest_installed_version()
            .filter(|v| !v.same_identity(&latest))
            .map(|v| v.folder_path());

        let cancel = Cancel::new();
        self.downloads.insert(
            key.clone(),
            DownloadState {
                cancel: cancel.clone(),
                fraction: 0.0,
                message: i18n::tr("Preparing"),
            },
        );

        tasks::spawn_download(
            &self.runtime,
            latest,
            previous_folder,
            self.tx.clone(),
            cancel,
        );
    }

    /// Stop every addon/patch/exe download, as switching mods does in C#.
    pub fn cancel_dependent_downloads(&mut self) {
        for (key, state) in &self.downloads {
            if key.kind != ModificationType::Mod {
                state.cancel.cancel();
            }
        }
    }

    // -- shutdown ----------------------------------------------------------

    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    pub fn request_quit(&mut self) {
        self.quit_requested = true;
    }

    // -- options ----------------------------------------------------------

    pub fn apply_default_options(&mut self) {
        match GameOptions::load(self.session.game_mode) {
            Ok(mut options) => {
                if let Err(e) = options.apply_defaults(primary_screen_size()) {
                    log::warn!("could not write Options.ini: {e:#}");
                }
            }
            Err(e) => log::warn!("could not read Options.ini: {e:#}"),
        }

        let data = &mut self.store.data;
        data.modded_exe = true;
        data.camera_height = 0;
        data.auto_update_gentool = true;
        data.check_mod_files = true;
        data.ask_before_check = true;
        data.windowed = true;
        data.use_vulkan = false;
        data.first_start = false;

        if let Err(e) = gentool::set_recommended_window_options() {
            log::warn!("could not update d3d8.cfg: {e:#}");
        }
        self.store.save();
    }

    // -- launch flow -------------------------------------------------------

    /// Entry point for the LAUNCH GAME and WORLD BUILDER buttons.
    pub fn request_launch(&mut self, world_builder: bool) {
        if world_builder && self.world_builder_running {
            self.show_dialog(Dialog::error(
                i18n::tr("LaunchAborted"),
                i18n::tr("WorldBuilderRunning"),
            ));
            return;
        }
        if !world_builder && self.game_running {
            self.show_dialog(Dialog::error(
                i18n::tr("LaunchAborted"),
                i18n::tr("GameRunning"),
            ));
            return;
        }

        self.advance_launch(world_builder, LaunchStep::Validate);
    }

    fn advance_launch(&mut self, world_builder: bool, step: LaunchStep) {
        match step {
            LaunchStep::Validate => {
                if let Some(error) = self.validate_launch(world_builder) {
                    self.show_dialog(error);
                    return;
                }
                self.advance_launch(world_builder, LaunchStep::FirstRun);
            }

            LaunchStep::FirstRun => {
                // Only the game offers the first-run defaults prompt.
                if !world_builder && self.store.data.first_start {
                    self.show_dialog(Dialog::confirm(
                        DialogId::ApplyDefaults { world_builder },
                        i18n::tr("FirstRun"),
                        i18n::tr("ApplyDefaultSettings"),
                        i18n::tr("Apply"),
                    ));
                    return;
                }
                self.advance_launch(world_builder, LaunchStep::CheckUpdates);
            }

            LaunchStep::CheckUpdates => {
                if let Some(message) = self.pending_update_message() {
                    self.show_dialog(Dialog::confirm(
                        DialogId::ConfirmUpdates { world_builder },
                        i18n::tr("ModificationsWithUpdate"),
                        message,
                        i18n::tr("Continue"),
                    ));
                    return;
                }
                self.advance_launch(world_builder, LaunchStep::CheckDeprecated);
            }

            LaunchStep::CheckDeprecated => {
                if let Some(message) = self.deprecated_message() {
                    self.show_dialog(Dialog::confirm(
                        DialogId::ConfirmDeprecated { world_builder },
                        i18n::tr("Compatibility"),
                        message,
                        i18n::tr("Continue"),
                    ));
                    return;
                }
                self.advance_launch(world_builder, LaunchStep::CheckIntegrity);
            }

            LaunchStep::CheckIntegrity => {
                if !self.integrity_check_possible() {
                    self.start_launch(world_builder, false);
                    return;
                }
                if !self.store.data.ask_before_check {
                    self.start_launch(world_builder, true);
                    return;
                }
                self.show_dialog(Dialog::confirm(
                    DialogId::ConfirmIntegrity { world_builder },
                    i18n::tr("CheckIntegrety"),
                    i18n::tr("TakeLongTime"),
                    i18n::tr("CheckFiles"),
                ));
            }
        }
    }

    /// The blocking checks that abort a launch outright.
    fn validate_launch(&self, world_builder: bool) -> Option<Dialog> {
        let aborted = i18n::tr("LaunchAborted");

        // Nothing may still be downloading.
        for key in self.downloads.keys() {
            let involved = match key.kind {
                ModificationType::Mod => self
                    .store
                    .selected_mod_name()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&key.name)),
                _ => self
                    .store
                    .active_versions()
                    .iter()
                    .any(|v| v.name().eq_ignore_ascii_case(&key.name)),
            };
            if involved {
                return Some(Dialog::error(
                    aborted,
                    i18n::trf("InstallInProgress", &[&key.name]),
                ));
            }
        }

        // Everything selected must actually be on disk.
        for version in self.store.active_versions() {
            if !version.installed {
                return Some(Dialog::error(
                    aborted,
                    i18n::trf("NotInstalled", &[version.name()]),
                ));
            }
        }

        // At most one executable may claim the game (or World Builder) slot.
        let claimants = self
            .store
            .selected_exe_versions()
            .iter()
            .filter(|v| {
                if world_builder {
                    v.info.replaces_original_wb_file
                } else {
                    v.info.replaces_original_game_file
                }
            })
            .count();

        if claimants > 1 {
            let key = if world_builder { "MoreThanOneWBExe" } else { "MoreThanOneGameExe" };
            return Some(Dialog::error(aborted, i18n::tr(key)));
        }

        None
    }

    /// Message naming the first selected modification with an uninstalled update.
    fn pending_update_message(&self) -> Option<String> {
        let mut candidates: Vec<&crate::model::GameModification> = Vec::new();
        candidates.extend(self.store.selected_mod());
        candidates.extend(self.store.selected_patch());
        candidates.extend(self.store.addons_for_selected_mod().into_iter().filter(|m| m.is_selected()));

        for modification in candidates {
            if let Some(latest) = modification.latest_version() {
                if !latest.installed {
                    return Some(i18n::trf("UninstalledUpdate", &[latest.name()]));
                }
            }
        }
        None
    }

    fn deprecated_message(&self) -> Option<String> {
        if let Some(patch) = self.store.selected_patch() {
            if patch.latest.info.deprecated {
                return Some(i18n::trf("Deprecated", &[patch.name()]));
            }
        }
        for addon in self.store.addons_for_selected_mod().into_iter().filter(|m| m.is_selected()) {
            if addon.latest.info.deprecated {
                return Some(i18n::trf("Deprecated", &[addon.name()]));
            }
        }
        None
    }

    /// Can the repository tell us what the installed files should look like?
    fn integrity_check_possible(&self) -> bool {
        if !self.store.data.check_mod_files || !self.store.connected {
            return false;
        }
        self.store
            .selected_mod_version()
            .is_some_and(|v| !v.info.s3_host_link.is_empty() && !v.info.s3_bucket_name.is_empty())
    }

    fn start_launch(&mut self, world_builder: bool, check_files: bool) {
        let versions = self.store.active_versions();
        if versions.is_empty() && self.store.selected_mod().is_some() {
            return;
        }

        self.busy = true;
        if world_builder {
            self.world_builder_running = true;
        } else {
            self.game_running = true;
            self.store.data.first_start = false;
        }
        self.store.save();

        tasks::spawn_launch(
            tasks::LaunchRequest {
                world_builder,
                versions,
                session: self.session.clone(),
                camera_height: self.store.data.camera_height,
                has_selected_mod: self.store.selected_mod().is_some(),
                check_files,
                windowed: self.store.data.windowed,
                quick_start: self.store.data.quick_start,
                game_params: self.store.data.game_params.clone(),
                use_vulkan: self.store.data.use_vulkan,
                gentool_auto_update: self.store.data.auto_update_gentool,
                vulkan: self.store.repos.vulkan.clone(),
            },
            self.tx.clone(),
        );
    }

    // -- tab switching -----------------------------------------------------

    /// Switch tabs, pulling in repository data for the base game on demand.
    pub fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;

        let needs_base_game_data = matches!(tab, Tab::Patches | Tab::Addons | Tab::Exes)
            && self.store.selected_mod().is_none()
            && self.store.connected
            && !self.store.repos.details_loaded(config::ORIGINAL_GAME_ALIAS);

        if needs_base_game_data {
            self.busy = true;
            tasks::spawn_original_game_extras(
                &self.runtime,
                tasks::StoreSnapshot::of(&self.store),
                self.tx.clone(),
            );
        }
    }

    /// Select a mod row, loading its patches and addons the first time.
    pub fn select_mod(&mut self, name: &str, ctx: &egui::Context) {
        let already = self.store.selected_mod_name();
        if already.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            return;
        }

        self.cancel_dependent_downloads();
        self.store.unselect_all_mods();

        if let Some(modification) = self.store.modification_mut(ModificationType::Mod, name) {
            modification.set_selected(true);
            // Make sure exactly one version is marked, as the combo box implies.
            if !modification.versions.iter().any(|v| v.is_selected && v.installed) {
                if let Some(pick) = modification.display_version().map(|v| v.version().to_owned()) {
                    for v in &mut modification.versions {
                        v.is_selected = v.info.version == pick;
                    }
                }
            }
        }

        if self.store.connected && !self.store.repos.details_loaded(name) {
            self.busy = true;
            tasks::spawn_mod_details(
                &self.runtime,
                tasks::StoreSnapshot::of(&self.store),
                name.to_owned(),
                self.tx.clone(),
            );
        }

        self.refresh_theme(ctx);
        self.store.save();
    }

    pub fn deselect_mod(&mut self, ctx: &egui::Context) {
        self.cancel_dependent_downloads();
        self.store.unselect_all_mods();
        self.refresh_theme(ctx);
        self.store.save();
    }

    /// Reorder the mod list by drag and drop.
    pub fn move_mod(&mut self, from: usize, to: usize) {
        if reorder(&mut self.store.data.modifications, from, to) {
            self.renumber_mods();
            self.store.save();
        }
    }

    /// Mods in the order the user arranged them.
    pub fn ordered_mod_names(&self) -> Vec<String> {
        // `data.modifications` is kept in display order by `sort_mods_by_saved_order`,
        // so a row's position here is also its index in the vector. Drag-and-drop
        // relies on that.
        self.store.data.modifications.iter().map(|m| m.name().to_owned()).collect()
    }

    /// Put the stored list into the order the user last arranged it in.
    /// Called once the saved config has been loaded.
    fn sort_mods_by_saved_order(&mut self) {
        self.store.data.modifications.sort_by_key(|m| m.number_in_list);
        self.renumber_mods();
    }

    /// Delete one installed version from the version drop-down.
    pub fn delete_version(&mut self, kind: ModificationType, name: &str, version: &str) {
        let Some(modification) = self.store.modification(kind, name) else { return };
        let Some(target) =
            modification.versions.iter().find(|v| v.version() == version).cloned()
        else {
            return;
        };

        if let Err(e) = self.store.delete_version(&target) {
            self.show_dialog(Dialog::error(i18n::tr("OperationAborted"), format!("{e:#}")));
            return;
        }
        self.store.save();
    }

    /// Newest-first list of a modification's installed versions.
    pub fn installed_versions(&self, kind: ModificationType, name: &str) -> Vec<String> {
        self.store
            .modification(kind, name)
            .map(|m| {
                let mut versions: Vec<String> = m
                    .versions
                    .iter()
                    .filter(|v| v.installed)
                    .map(|v| v.version().to_owned())
                    .collect();
                versions.sort_by(|a, b| compare_versions(b, a));
                versions
            })
            .unwrap_or_default()
    }
}

/// Move the item at `from` so it lands on the slot the drop marker showed:
/// after `to` when dragging down, before `to` when dragging up.
///
/// Both directions reduce to "remove, then insert at `to`", because removing an
/// earlier element shifts everything after it down by one.
fn reorder<T>(items: &mut Vec<T>, from: usize, to: usize) -> bool {
    if from == to || from >= items.len() || to >= items.len() {
        return false;
    }
    let entry = items.remove(from);
    items.insert(to.min(items.len()), entry);
    true
}

#[derive(Debug, Clone, Copy)]
enum LaunchStep {
    Validate,
    FirstRun,
    CheckUpdates,
    CheckDeprecated,
    CheckIntegrity,
}

/// Best guess at the primary display size, for the recommended resolution.
pub fn primary_screen_size() -> (u32, u32) {
    (1920, 1080)
}

#[cfg(test)]
mod tests {
    use super::reorder;

    fn order(from: usize, to: usize) -> Vec<&'static str> {
        let mut items = vec!["A", "B", "C", "D"];
        reorder(&mut items, from, to);
        items
    }

    #[test]
    fn dragging_down_lands_after_the_target() {
        assert_eq!(order(0, 1), vec!["B", "A", "C", "D"]);
        assert_eq!(order(0, 3), vec!["B", "C", "D", "A"]);
        assert_eq!(order(1, 2), vec!["A", "C", "B", "D"]);
    }

    #[test]
    fn dragging_up_lands_before_the_target() {
        assert_eq!(order(3, 0), vec!["D", "A", "B", "C"]);
        assert_eq!(order(2, 1), vec!["A", "C", "B", "D"]);
    }

    #[test]
    fn a_no_op_or_out_of_range_move_changes_nothing() {
        let mut items = vec!["A", "B"];
        assert!(!reorder(&mut items, 1, 1));
        assert!(!reorder(&mut items, 5, 0));
        assert!(!reorder(&mut items, 0, 5));
        assert_eq!(items, vec!["A", "B"]);
    }
}

