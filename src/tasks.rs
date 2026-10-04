//! Background work and the messages it sends back to the UI thread.
//!
//! Every long-running operation the WPF build ran with `async void` or
//! `Task.Run` lives here. The UI never blocks: it drains [`Bg`] messages once
//! per frame and updates its own state.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::config::{self, Game, SessionInfo};
use crate::game::mod_archive::InstallReport;
use crate::game::{self, launcher, stock_files};
use crate::model::repos::VulkanData;
use crate::model::{ModVersion, ModificationType, ReposVersion, ProtonSettings};
use crate::net::download::{Cancel, DownloadEvent, DownloadResult, ModKey};
use crate::net::{self, download, manifests};
use crate::state::{self, Store};
use crate::util::fs as gfs;

/// A message from a worker to the UI.
pub enum Bg {
    /// Splash-screen status line.
    InitStatus(String),
    /// Start-up finished; the store is ready to use.
    InitDone(Box<Store>),
    /// Progress or completion of a modification download.
    Download(DownloadEvent),
    /// Patch/addon/executable manifests for one mod arrived.
    ModDetails { mod_name: String, versions: Vec<ReposVersion> },
    /// A mod the user picked in the "add mod" dialog was resolved.
    ModAdded { version: Box<ReposVersion>, extras: Vec<ReposVersion> },
    /// Patches and addons for the base game arrived.
    OriginalGameExtras(Vec<ReposVersion>),
    /// One line of progress while files are being added by hand.
    ManualAddStatus(String),
    /// Files were unpacked into the mod store by hand; rescan is needed.
    /// `archives` are the archives that were installed, so the user can be
    /// offered their deletion; it is empty when the install failed.
    ManualAddDone { error: Option<String>, archives: Vec<PathBuf>, report: InstallReport },
    /// Progress of the side-bar download (the Vulkan layer).
    SideProgress { label: String, total: Option<u64>, read: u64 },
    /// The side-bar download finished.
    SideDone { error: Option<String> },
    /// The pre-launch integrity check and file linking finished.
    GamePrepared { world_builder: bool, ok: bool },
    /// The game or World Builder exited.
    GameFinished { world_builder: bool, played_long_enough: bool, error: Option<String> },
    /// Something the user should see as a modal error.
    Error { title: String, message: String },
}

// ---------------------------------------------------------------------------
// Start-up
// ---------------------------------------------------------------------------

/// Everything `InitWindow.PrepareLauncher` did, off the UI thread.
pub async fn initialize(game: Game, tx: Sender<Bg>) {
    let status = |text: &str| {
        let _ = tx.send(Bg::InitStatus(text.to_owned()));
    };

    status(&crate::i18n::tr("Preparing"));

    // -- local housekeeping ------------------------------------------------
    let local = tokio::task::spawn_blocking(prepare_local_folders).await;
    if let Err(e) = local {
        log::error!("local preparation panicked: {e}");
    }

    let mut store = Store::new(game);
    store.game_mode = game;

    let store = tokio::task::spawn_blocking(move || {
        store.purge_advertising();
        store.refresh_local_modifications();
        store
    })
    .await;

    let mut store = match store {
        Ok(s) => s,
        Err(e) => {
            log::error!("scanning the mod folder panicked: {e}");
            Store::new(game)
        }
    };

    // -- network -----------------------------------------------------------
    let repos_url = game.repos_url();
    let connected = net::check_connection(repos_url).await;
    store.connected = connected;

    if connected {
        status("Reading repository index...");
        match manifests::fetch_repos_index(repos_url).await {
            Ok(index) => {
                store.repos.mod_names = index.mod_datas.iter().map(|m| m.mod_name.clone()).collect();

                if !index.vulkan_repos_data.is_empty() {
                    store.repos.vulkan = load_vulkan(&index.vulkan_repos_data).await;
                }

                // Only mods the user already has are refreshed at start-up.
                let installed: Vec<String> = store
                    .data
                    .modifications
                    .iter()
                    .map(|m| m.name().to_ascii_lowercase())
                    .collect();

                for entry in &index.mod_datas {
                    if !installed.contains(&entry.mod_name.to_ascii_lowercase()) {
                        continue;
                    }
                    status(&format!("Updating {}...", entry.mod_name));
                    match manifests::fetch_modification(&entry.mod_link).await {
                        Ok(version) => {
                            download_images(&version).await;
                            store.add_repos_version(version);
                        }
                        Err(e) => log::warn!("skipping {}: {e:#}", entry.mod_name),
                    }
                }

                // Executables that are not tied to a mod are always offered.
                for entry in index.executables.iter().filter(|e| e.dependency_name.is_empty()) {
                    match manifests::fetch_modification(&entry.mod_link).await {
                        Ok(mut version) => {
                            version.modification_type = ModificationType::Executable;
                            if version.dependence_name.is_empty() {
                                version.dependence_name = config::ORIGINAL_GAME_ALIAS.to_owned();
                            }
                            store.add_repos_version(version);
                        }
                        Err(e) => log::warn!("skipping executable {}: {e:#}", entry.mod_name),
                    }
                }

                store.repos.index = index;
            }
            Err(e) => {
                log::warn!("cannot read the repository index: {e:#}");
                store.connected = false;
            }
        }
    }

    if store.connected {
        if let Some(selected) = store.selected_mod_name() {
            status(&format!("Loading add-ons for {selected}..."));
            let extras = fetch_mod_details(&store, &selected).await;
            store.repos.mark_details_loaded(&selected);
            for version in extras {
                store.add_repos_version(version);
            }
        }
    }

    // Registering repository versions can resurrect folders that no longer
    // exist, so reconcile with disk once more.
    let mut store = tokio::task::spawn_blocking(move || {
        store.refresh_local_modifications();
        store
    })
    .await
    .unwrap_or_else(|_| Store::new(game));

    if !store.data.modifications.is_empty() {
        store.data.first_start = false;
    }

    let _ = tx.send(Bg::InitDone(Box::new(store)));
}

fn prepare_local_folders() {
    if let Err(e) = state::create_launcher_folder() {
        log::warn!("cannot create the launcher folder: {e}");
    }

    for folder in [config::MODS_FOLDER, config::VULKAN_DLLS_FOLDER_NAME] {
        let path = config::game_path(folder);
        if let Err(e) = std::fs::create_dir_all(&path) {
            log::warn!("cannot create {}: {e}", path.display());
        }
    }

    migrate_old_mods_folder();

    // Undo anything a previous run left behind before touching the folder.
    launcher::restore_game_folder();
    launcher::delete_temp_folders(config::game_dir());
    delete_old_launcher_binary();
    shadow_conflicting_dlls();

    let images = config::game_path(config::LAUNCHER_FOLDER)
        .join(config::LAUNCHER_IMAGE_SUBFOLDER);
    let _ = std::fs::create_dir_all(&images);

    extract_placeholder_image();
    let _ = game::gentool::extract_default_cfg();
}

/// Move a pre-1.0 `GenLauncherModifications/` store into `GLM/`.
fn migrate_old_mods_folder() {
    let old = config::game_path(config::MODS_FOLDER_OLD);
    if !old.is_dir() {
        return;
    }
    let new = config::game_path(config::MODS_FOLDER);

    let Ok(entries) = std::fs::read_dir(&old) else { return };
    for entry in entries.flatten() {
        let target = new.join(entry.file_name());
        if target.exists() {
            continue;
        }
        let result = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            gfs::move_dir(&entry.path(), &target)
        } else {
            gfs::move_file(&entry.path(), &target)
        };
        if let Err(e) = result {
            log::warn!("could not migrate {}: {e}", entry.path().display());
        }
    }
    let _ = std::fs::remove_dir(&old);
}

/// Remove the renamed binary left over from a self-update.
fn delete_old_launcher_binary() {
    if let Ok(exe) = std::env::current_exe() {
        let old = PathBuf::from(format!("{}Old", exe.display()));
        let _ = std::fs::remove_file(old);
    }
    // A crash inside the game leaves this behind and breaks the next launch.
    let _ = std::fs::remove_file(config::game_path("dbghelp.dll"));
}

/// Graphics wrappers in the game folder fight with the ones we link in.
fn shadow_conflicting_dlls() {
    for name in ["d3d8x.dll", "d3d9.dll", "d3d10core.dll", "d3d11.dll"] {
        let path = config::game_path(name);
        if path.exists() && !gfs::is_symlink(&path) {
            let target = config::game_path(format!("{name}{}", config::REPLACE_SUFFIX));
            let _ = std::fs::remove_file(&target);
            let _ = gfs::move_file(&path, &target);
        }
    }
}

const UAM_ZH: &[u8] = include_bytes!("../assets/images/uamZH.jpg");
const UAM_GEN: &[u8] = include_bytes!("../assets/images/uamG.jpg");

/// The stock artwork shown for a mod that has none of its own.
fn extract_placeholder_image() {
    let path = placeholder_image_path();
    if path.exists() {
        return;
    }
    let bytes = match detect_game().unwrap_or(Game::ZeroHour) {
        Game::ZeroHour => UAM_ZH,
        Game::Generals => UAM_GEN,
    };
    let _ = std::fs::write(path, bytes);
}

pub fn placeholder_image_path() -> PathBuf {
    config::game_path(config::LAUNCHER_FOLDER).join("uam.jpg")
}

/// Which game is installed here? Decided by the window archive present.
pub fn detect_game() -> Option<Game> {
    let has = |name: &str| {
        config::game_path(name).exists()
            || config::game_path(format!("{name}{}", config::REPLACE_SUFFIX)).exists()
    };
    if has("WindowZH.big") {
        Some(Game::ZeroHour)
    } else if has("Window.big") {
        Some(Game::Generals)
    } else {
        None
    }
}

pub fn session_info(game: Game, connected: bool) -> SessionInfo {
    SessionInfo { connected, game_mode: game, game_files: stock_files::for_game(game) }
}

async fn load_vulkan(url: &str) -> Option<VulkanData> {
    manifests::fetch_vulkan_data(url).await.ok()
}

/// Download a mod's logo and custom background, once each.
async fn download_images(version: &ReposVersion) {
    let folder = config::game_path(config::LAUNCHER_FOLDER)
        .join(config::LAUNCHER_IMAGE_SUBFOLDER)
        .join(gfs::sanitize_file_name(&version.name));

    if !version.ui_image_source_link.is_empty() {
        let _ = net::download_file_if_missing(
            &version.ui_image_source_link,
            &folder,
            &version.version,
        )
        .await;
    }
}

/// Patch, addon and executable manifests for one mod.
async fn fetch_mod_details(store: &Store, mod_name: &str) -> Vec<ReposVersion> {
    let Some(raw) = store.repos.raw_data_for(mod_name) else { return Vec::new() };

    let mut versions = Vec::new();

    for (urls, kind) in [
        (raw.mod_patches.clone(), ModificationType::Patch),
        (raw.mod_addons.clone(), ModificationType::Addon),
    ] {
        for mut version in manifests::fetch_modifications(&urls).await {
            version.modification_type = kind;
            if version.dependence_name.is_empty() {
                version.dependence_name = mod_name.to_owned();
            }
            versions.push(version);
        }
    }

    let exe_links: Vec<String> = store
        .repos
        .index
        .executables
        .iter()
        .filter(|e| e.dependency_name.eq_ignore_ascii_case(mod_name))
        .map(|e| e.mod_link.clone())
        .collect();

    for mut version in manifests::fetch_modifications(&exe_links).await {
        version.modification_type = ModificationType::Executable;
        if version.dependence_name.is_empty() {
            version.dependence_name = mod_name.to_owned();
        }
        versions.push(version);
    }

    versions
}

/// Load patches/addons/executables for `mod_name` in the background.
pub fn spawn_mod_details(
    runtime: &tokio::runtime::Runtime,
    store_snapshot: StoreSnapshot,
    mod_name: String,
    tx: Sender<Bg>,
) {
    runtime.spawn(async move {
        let versions = store_snapshot.fetch_details(&mod_name).await;
        let _ = tx.send(Bg::ModDetails { mod_name, versions });
    });
}

/// The slice of repository data a detail fetch needs, owned so it can cross
/// threads without borrowing the store.
pub struct StoreSnapshot {
    pub index: crate::model::repos::ReposModsData,
}

impl StoreSnapshot {
    pub fn of(store: &Store) -> Self {
        Self { index: store.repos.index.clone() }
    }

    async fn fetch_details(&self, mod_name: &str) -> Vec<ReposVersion> {
        let Some(raw) =
            self.index.mod_datas.iter().find(|m| m.mod_name.eq_ignore_ascii_case(mod_name))
        else {
            return Vec::new();
        };

        let mut versions = Vec::new();
        for (urls, kind) in [
            (raw.mod_patches.clone(), ModificationType::Patch),
            (raw.mod_addons.clone(), ModificationType::Addon),
        ] {
            for mut version in manifests::fetch_modifications(&urls).await {
                version.modification_type = kind;
                if version.dependence_name.is_empty() {
                    version.dependence_name = mod_name.to_owned();
                }
                versions.push(version);
            }
        }

        let exe_links: Vec<String> = self
            .index
            .executables
            .iter()
            .filter(|e| e.dependency_name.eq_ignore_ascii_case(mod_name))
            .map(|e| e.mod_link.clone())
            .collect();

        for mut version in manifests::fetch_modifications(&exe_links).await {
            version.modification_type = ModificationType::Executable;
            if version.dependence_name.is_empty() {
                version.dependence_name = mod_name.to_owned();
            }
            versions.push(version);
        }

        versions
    }

    async fn fetch_original_game_extras(&self) -> Vec<ReposVersion> {
        let mut versions = Vec::new();
        for (urls, kind) in [
            (self.index.original_game_patches.clone(), ModificationType::Patch),
            (self.index.original_game_addons.clone(), ModificationType::Addon),
        ] {
            for mut version in manifests::fetch_modifications(&urls).await {
                version.modification_type = kind;
                version.dependence_name = config::ORIGINAL_GAME_ALIAS.to_owned();
                versions.push(version);
            }
        }
        versions
    }
}

/// Load the base game's own patches and addons.
pub fn spawn_original_game_extras(
    runtime: &tokio::runtime::Runtime,
    snapshot: StoreSnapshot,
    tx: Sender<Bg>,
) {
    runtime.spawn(async move {
        let versions = snapshot.fetch_original_game_extras().await;
        let _ = tx.send(Bg::OriginalGameExtras(versions));
    });
}

/// Resolve a mod the user picked from the "add mod" dialog, then its extras.
pub fn spawn_add_mod(
    runtime: &tokio::runtime::Runtime,
    snapshot: StoreSnapshot,
    mod_name: String,
    tx: Sender<Bg>,
) {
    runtime.spawn(async move {
        let Some(entry) =
            snapshot.index.mod_datas.iter().find(|m| m.mod_name.eq_ignore_ascii_case(&mod_name))
        else {
            let _ = tx.send(Bg::Error {
                title: crate::i18n::tr("OperationAborted"),
                message: format!("{mod_name} is not in the repository"),
            });
            return;
        };

        match manifests::fetch_modification(&entry.mod_link).await {
            Ok(version) => {
                download_images(&version).await;
                let extras = snapshot.fetch_details(&version.name).await;
                let _ = tx.send(Bg::ModAdded { version: Box::new(version), extras });
            }
            Err(e) => {
                let _ = tx.send(Bg::Error {
                    title: crate::i18n::tr("OperationAborted"),
                    message: format!("{e:#}"),
                });
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Modification downloads
// ---------------------------------------------------------------------------

/// Start downloading a modification, picking the engine its manifest implies.
pub fn spawn_download(
    runtime: &tokio::runtime::Runtime,
    version: ModVersion,
    previous_version_folder: Option<PathBuf>,
    tx: Sender<Bg>,
    cancel: Cancel,
) {
    let use_s3 =
        !version.info.s3_host_link.is_empty() && !version.info.s3_folder_name.is_empty();

    let key = ModKey::of(&version);
    let (event_tx, event_rx) = std::sync::mpsc::channel::<DownloadEvent>();

    // Forward engine events onto the single UI channel.
    let forward_tx = tx.clone();
    std::thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            if forward_tx.send(Bg::Download(event)).is_err() {
                break;
            }
        }
    });

    runtime.spawn(async move {
        let result: DownloadResult = if use_s3 {
            download::s3_multi_file(version, previous_version_folder, event_tx.clone(), cancel)
                .await
        } else {
            download::http_single_file(version, event_tx.clone(), cancel).await
        };
        let _ = event_tx.send(DownloadEvent::Done { key, result });
    });
}

/// Check whether the system clock is far enough off to break S3 signatures.
pub fn system_time_is_out_of_sync() -> bool {
    crate::util::ntp::is_system_time_out_of_sync()
}

// ---------------------------------------------------------------------------
// Launching the game
// ---------------------------------------------------------------------------

pub struct LaunchRequest {
    pub world_builder: bool,
    pub versions: Vec<ModVersion>,
    pub session: SessionInfo,
    pub camera_height: i32,
    pub has_selected_mod: bool,
    pub check_files: bool,
    pub windowed: bool,
    pub quick_start: bool,
    pub game_params: String,
    pub use_vulkan: bool,
    pub gentool_auto_update: bool,
    /// The repository's modded game executable, to download if needed and run
    /// in place of the stock one ("Use modded exe files").
    pub modded_exe: Option<ModVersion>,
    /// Set when the repository advertises a Vulkan layer, so the launch can
    /// refresh it before starting the game.
    pub vulkan: Option<VulkanData>,
    /// How Proton runs the game off Windows.
    pub proton: ProtonSettings,
}

/// Verify, link, run, unlink. Runs on its own thread; the game call blocks.
///
/// A panic on this thread must still report back: the UI treats the game as
/// running, and refuses to close, until it hears `GameFinished`.
pub fn spawn_launch(request: LaunchRequest, tx: Sender<Bg>) {
    std::thread::spawn(move || {
        let world_builder = request.world_builder;
        let panic_tx = tx.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || run_launch(request, tx)));
        if let Err(panic) = outcome {
            let reason = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            log::error!("the launch thread panicked: {reason}");
            launcher::restore_game_folder();
            let _ = panic_tx.send(Bg::GameFinished {
                world_builder,
                played_long_enough: false,
                error: Some(format!("The launch failed unexpectedly: {reason}")),
            });
        }
    });
}

fn run_launch(request: LaunchRequest, tx: Sender<Bg>) {
    let LaunchRequest {
        world_builder,
        versions,
        session,
        camera_height,
        has_selected_mod,
        check_files,
        windowed,
        quick_start,
        game_params,
        use_vulkan,
        gentool_auto_update,
        modded_exe,
        vulkan,
        proton,
    } = request;

    // A prefix Proton has not created yet has no `Options.ini`, and the
    // game crashes on start without one; this writes the stock file.
    // Only with a prefix to put it in: otherwise it would land in ~/Documents.
    if cfg!(not(windows)) && game::proton::prefix(&proton).is_some() {
        if let Err(e) = game::options::GameOptions::load(session.game_mode, &proton) {
            log::warn!("could not prepare Options.ini: {e:#}");
        }
    }

    let mut versions = versions;
    if let Some(exe) = modded_exe {
        match ensure_executable(&exe, session.connected, &tx) {
            Ok(()) => versions.push(exe),
            Err(e) => log::warn!("running the stock executable: {e:#}"),
        }
    }

    // The integrity check needs the mod linked in, then unlinked again.
    if check_files && session.connected {
        if let Some(mod_version) =
            versions.iter().find(|v| v.kind() == ModificationType::Mod).cloned()
        {
            let ok = verify_mod_files(&mod_version, &session, has_selected_mod);
            launcher::restore_game_folder();
            if !ok {
                let _ = tx.send(Bg::GamePrepared { world_builder, ok: false });
                return;
            }
        }
    }

    launcher::prepare_game_files(
        &versions,
        &session,
        camera_height,
        has_selected_mod,
        true,
        false,
    );

    let _ = tx.send(Bg::GamePrepared { world_builder, ok: true });

    if !world_builder {
        if gentool_auto_update {
            if session.connected {
                update_gentool(&tx);
            }
        } else {
            // Restoring the folder after the last run put it back.
            game::gentool::shadow_dll();
        }
    }

    // The Vulkan layer is refreshed and linked in after the mod files are
    // in place, exactly where `CheckAndUpdateVulkan` sat in the C# build.
    if use_vulkan && !world_builder {
        update_vulkan_layer(session.connected, vulkan.as_ref(), &tx);
        game::vulkan::create_symlinks(gentool_auto_update);
    }

    let result = if world_builder {
        launcher::run_world_builder(&versions, &proton).map(|()| false)
    } else {
        launcher::run_game(&versions, windowed, quick_start, &game_params, &proton)
            .map(|o| o.played_long_enough)
    };

    launcher::restore_game_folder();

    match result {
        Ok(played_long_enough) => {
            let _ = tx.send(Bg::GameFinished {
                world_builder,
                played_long_enough,
                error: None,
            });
        }
        Err(e) => {
            let _ = tx.send(Bg::GameFinished {
                world_builder,
                played_long_enough: false,
                error: Some(format!("{e:#}")),
            });
        }
    }

}

/// Fetch a newer Vulkan layer if the repository has one. Runs on the launch
/// thread so the game only starts once the DLLs are in place.
fn update_vulkan_layer(connected: bool, vulkan: Option<&VulkanData>, tx: &Sender<Bg>) {
    let folder = game::vulkan::folder();
    let _ = std::fs::create_dir_all(&folder);

    if !connected {
        return;
    }
    let Some(vulkan) = vulkan else { return };
    if !game::vulkan::is_outdated(Some(vulkan)) {
        return;
    }

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return;
    };

    let progress_tx = tx.clone();
    let result = runtime.block_on(download::simple_file(
        &vulkan.download_link,
        &folder,
        true,
        move |total, read| {
            let _ = progress_tx.send(Bg::SideProgress {
                label: "Vulkan".to_owned(),
                total,
                read,
            });
        },
    ));

    let error = result.err().map(|e| format!("{e:#}"));
    let _ = tx.send(Bg::SideDone { error });
}

/// Download an executable from the repository unless it is already on disk.
fn ensure_executable(exe: &ModVersion, connected: bool, tx: &Sender<Bg>) -> anyhow::Result<()> {
    let folder = exe.folder_path();
    let file = gfs::resolve_case_insensitive(&folder, exe.info.executable_file_name.as_ref());
    if file.is_file() {
        return Ok(());
    }
    anyhow::ensure!(connected, "{} is not downloaded and the launcher is offline", exe.name());
    anyhow::ensure!(!exe.info.simple_download_link.is_empty(), "{} has no download link", exe.name());

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let progress_tx = tx.clone();
    let label = exe.name().to_owned();
    let result = runtime.block_on(download::simple_file(
        &exe.info.simple_download_link,
        &folder,
        true,
        move |total, read| {
            let _ = progress_tx.send(Bg::SideProgress { label: label.clone(), total, read });
        },
    ));
    let _ = tx.send(Bg::SideDone { error: result.as_ref().err().map(|e| format!("{e:#}")) });
    result?;

    anyhow::ensure!(file.is_file(), "the {} download has no {}", exe.name(), file.display());
    Ok(())
}

/// Install GenTool's `d3d8.dll`, or replace it when gentool.net has a newer
/// one. An unreachable site just leaves the current copy in place.
fn update_gentool(tx: &Sender<Bg>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return;
    };
    let latest = match runtime.block_on(net::gentool::latest_version()) {
        Ok(latest) => latest,
        Err(e) => {
            log::warn!("cannot check for GenTool updates: {e:#}");
            return;
        }
    };
    if !game::gentool::is_outdated(game::gentool::current_version(), &latest) {
        return;
    }

    let staging = config::game_path(config::LAUNCHER_FOLDER).join("GenTool");
    let _ = std::fs::remove_dir_all(&staging);

    let progress_tx = tx.clone();
    let result = runtime
        .block_on(download::simple_file(
            &game::gentool::download_link(&latest),
            &staging,
            true,
            move |total, read| {
                let _ = progress_tx.send(Bg::SideProgress { label: "GenTool".to_owned(), total, read });
            },
        ))
        .and_then(|_| game::gentool::install_dll_from(&staging));
    let _ = std::fs::remove_dir_all(&staging);

    if let Err(e) = &result {
        log::warn!("could not install GenTool {latest}: {e:#}");
    }
    let _ = tx.send(Bg::SideDone { error: result.err().map(|e| format!("GenTool: {e:#}")) });
}

/// Link just the mod, compare its files against the repository, and report.
fn verify_mod_files(
    mod_version: &ModVersion,
    session: &SessionInfo,
    has_selected_mod: bool,
) -> bool {
    if mod_version.info.s3_host_link.is_empty() || mod_version.info.s3_bucket_name.is_empty() {
        return true;
    }

    launcher::prepare_game_files(
        std::slice::from_ref(mod_version),
        session,
        0,
        has_selected_mod,
        false,
        true,
    );

    // A listing we cannot fetch must not block the launch.
    let listing = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()
        .and_then(|rt| rt.block_on(net::s3::list_mod_files(&mod_version.info)).ok());

    match listing {
        Some(files) => launcher::mod_files_are_correct(&files),
        None => true,
    }
}

/// Install the files the user picked into the mod store, unpacking archives.
pub fn spawn_manual_add(
    runtime: &tokio::runtime::Runtime,
    files: Vec<PathBuf>,
    target_relative: String,
    game: Game,
    tx: Sender<Bg>,
) {
    runtime.spawn_blocking(move || {
        let target = config::game_path(&target_relative);
        let status_tx = tx.clone();
        let status = move |text: &str| {
            let _ = status_tx.send(Bg::ManualAddStatus(text.to_owned()));
        };

        let message = match install_files(&files, &target, game, &status) {
            Ok(report) => Bg::ManualAddDone {
                error: None,
                archives: files
                    .into_iter()
                    .filter(|f| crate::util::archive::is_supported_archive(f))
                    .collect(),
                report,
            },
            Err(e) => Bg::ManualAddDone {
                error: Some(format!("{e:#}")),
                archives: Vec::new(),
                report: InstallReport::default(),
            },
        };
        let _ = tx.send(message);
    });
}

/// Port of `ModificationsFileHandler.ExtractModificationFromFiles`. Archives
/// are unpacked straight from where they are, never copied first.
fn install_files(
    files: &[PathBuf],
    target: &std::path::Path,
    game: Game,
    status: &dyn Fn(&str),
) -> anyhow::Result<InstallReport> {
    let mut report = InstallReport::default();

    for file in files {
        if crate::util::archive::is_supported_archive(file) {
            report.merge(&game::mod_archive::install(file, target, game, status)?);
            continue;
        }

        std::fs::create_dir_all(target)?;
        let copied = target.join(gfs::file_name_of(file));
        if !copied.exists() {
            std::fs::copy(file, &copied)?;
        }
        if gfs::extension_of(&copied) == "big" {
            // Stored as .gib so the game only sees it once it is linked in.
            let gib = gfs::change_extension(&copied, "gib");
            let _ = std::fs::remove_file(&gib);
            gfs::move_file(&copied, &gib)?;
        }
    }
    Ok(report)
}
