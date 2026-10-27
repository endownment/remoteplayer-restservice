use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::audio::AudioInventory;
use crate::cec::CecBackend;
use crate::config::AppConfig;
use crate::mpv::{MpvClient, OBSERVED_PROPERTIES};
use crate::power::PowerControl;
use crate::service_manager::ServiceManager;
use crate::settings::{PlayerSettings, SettingsStore};
use crate::storage::MediaLibrary;
use crate::watcher_client::WatcherClient;

/// Everything request handlers need, created once at startup and shared by
/// reference (`Arc<AppContext>`) between all requests — the Rust counterpart of
/// a dependency-injection container with singleton scope, wired by hand.
pub struct AppContext {
    pub config: AppConfig,
    pub mpv: MpvClient,
    pub settings: SettingsStore,
    pub library: MediaLibrary,
    pub audio: AudioInventory,
    pub service_manager: ServiceManager,
    pub cec: CecBackend,
    pub power: PowerControl,
    pub watcher: Option<WatcherClient>,
    /// Serializes operations that restart mpv or change settings.
    pub player_operation: tokio::sync::Mutex<()>,
    /// Set while this service restarts mpv itself, so crash recovery stays out of the way.
    pub planned_restart: AtomicBool,
}

impl AppContext {
    pub async fn initialize(config: AppConfig) -> Result<Arc<AppContext>, Box<dyn std::error::Error>> {
        let library: MediaLibrary = MediaLibrary::open(&config.storage)?;

        let defaults: PlayerSettings =
            PlayerSettings::builtin_defaults().with_patch(&config.player_defaults, &PlayerSettings::builtin_defaults());
        let settings: SettingsStore =
            SettingsStore::load(config.mpv.settings_file.clone(), config.mpv.args_env_file.clone(), defaults)?;
        match settings.ensure_env_file_matches_current() {
            Ok(true) => tracing::info!(
                file = %settings.args_env_file().display(),
                "mpv arguments file rewritten; it takes effect on the next mpv restart"
            ),
            Ok(false) => {}
            // A read-only system still runs, with the settings mpv already has.
            Err(error) => tracing::warn!(%error, "cannot update the mpv arguments file; settings cannot be changed"),
        }

        let observed: Vec<String> = OBSERVED_PROPERTIES.iter().map(|name: &&str| name.to_string()).collect();
        let mpv: MpvClient = MpvClient::spawn(
            config.mpv.ipc_socket.clone(),
            observed,
            Duration::from_secs(config.mpv.request_timeout_seconds),
        );

        let system_bus: Option<zbus::Connection> =
            if config.needs_system_bus() { Some(zbus::Connection::system().await?) } else { None };
        let service_manager: ServiceManager =
            ServiceManager::from_config(&config.service_manager, &config.mpv.ipc_socket, system_bus.clone());
        service_manager.start_if_managed_here(settings.current().to_mpv_args()).await?;

        let cec: CecBackend = CecBackend::from_config(&config.cec);
        let power: PowerControl = PowerControl::from_config(config.system.power, system_bus);
        let audio: AudioInventory = AudioInventory::new(config.mpv.executable.clone());
        let watcher: Option<WatcherClient> = config.restore.watcher_socket.clone().map(WatcherClient::new);

        Ok(Arc::new(AppContext {
            config,
            mpv,
            settings,
            library,
            audio,
            service_manager,
            cec,
            power,
            watcher,
            player_operation: tokio::sync::Mutex::new(()),
            planned_restart: AtomicBool::new(false),
        }))
    }
}
