//! Starting and restarting mpv.
//!
//! On the Pi mpv is the systemd unit `remoteplayer-mpv.service`; its arguments
//! come from the EnvironmentFile that `SettingsStore` writes, so a restart is
//! all that is needed to apply them. On a development machine the child-process
//! manager runs mpv itself and passes the arguments directly.

mod child_process;
mod systemd;

pub use child_process::ChildProcessManager;
pub use systemd::SystemdUnitManager;

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::config::ServiceManagerConfig;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("D-Bus call to systemd failed: {0}")]
    DBus(#[from] zbus::Error),
    #[error("cannot run {command}: {source}")]
    Spawn { command: String, source: std::io::Error },
    #[error("the mpv supervisor task has stopped")]
    SupervisorGone,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ServiceStatus {
    pub manager: &'static str,
    pub unit: Option<String>,
    pub active_state: String,
    pub sub_state: String,
    /// systemd `UnitFileState`: "enabled" means mpv starts at boot.
    pub autostart: Option<String>,
    pub main_pid: Option<u32>,
}

pub enum ServiceManager {
    Systemd(SystemdUnitManager),
    ChildProcess(ChildProcessManager),
}

impl ServiceManager {
    pub fn from_config(
        config: &ServiceManagerConfig,
        ipc_socket: &Path,
        system_bus: Option<zbus::Connection>,
    ) -> ServiceManager {
        match (config, system_bus) {
            (ServiceManagerConfig::Systemd { unit, journalctl }, Some(bus)) => {
                ServiceManager::Systemd(SystemdUnitManager::new(bus, unit.clone(), journalctl.clone()))
            }
            (ServiceManagerConfig::Systemd { .. }, None) => {
                unreachable!("AppContext connects to the system bus whenever the systemd manager is configured")
            }
            (ServiceManagerConfig::ChildProcess { command, fixed_args, respawn_delay_seconds }, _) => {
                ServiceManager::ChildProcess(ChildProcessManager::spawn(
                    command.clone(),
                    fixed_args.clone(),
                    ipc_socket.to_path_buf(),
                    Duration::from_secs(*respawn_delay_seconds),
                ))
            }
        }
    }

    /// systemd starts mpv by itself; the child-process manager needs a first start.
    pub async fn start_if_managed_here(&self, mpv_args: Vec<String>) -> Result<(), ServiceError> {
        match self {
            ServiceManager::Systemd(_) => Ok(()),
            ServiceManager::ChildProcess(manager) => manager.restart(mpv_args).await,
        }
    }

    pub async fn restart(&self, mpv_args: Vec<String>) -> Result<(), ServiceError> {
        match self {
            // The arguments already sit in the EnvironmentFile read by the unit.
            ServiceManager::Systemd(manager) => manager.restart().await,
            ServiceManager::ChildProcess(manager) => manager.restart(mpv_args).await,
        }
    }

    pub async fn status(&self) -> Result<ServiceStatus, ServiceError> {
        match self {
            ServiceManager::Systemd(manager) => manager.status().await,
            ServiceManager::ChildProcess(manager) => manager.status().await,
        }
    }

    pub async fn recent_log(&self, lines: usize) -> Result<Vec<String>, ServiceError> {
        match self {
            ServiceManager::Systemd(manager) => manager.recent_log(lines).await,
            ServiceManager::ChildProcess(manager) => Ok(manager.recent_log(lines)),
        }
    }
}
