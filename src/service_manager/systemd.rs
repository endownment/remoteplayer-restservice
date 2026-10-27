use std::path::PathBuf;

use zbus::proxy::CacheProperties;
use zbus::zvariant::OwnedObjectPath;

use crate::service_manager::{ServiceError, ServiceStatus};

// The `#[zbus::proxy]` macro turns each trait into a generated client struct
// (`SystemdManagerProxy`, ...) whose async methods perform the D-Bus calls.
// Method names are converted to D-Bus names: `restart_unit` -> `RestartUnit`.
// Interface reference: `man org.freedesktop.systemd1`.

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn load_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;
    fn restart_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(interface = "org.freedesktop.systemd1.Unit", default_service = "org.freedesktop.systemd1")]
trait SystemdUnit {
    #[zbus(property)]
    fn active_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn sub_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn unit_file_state(&self) -> zbus::Result<String>;
}

#[zbus::proxy(interface = "org.freedesktop.systemd1.Service", default_service = "org.freedesktop.systemd1")]
trait SystemdService {
    #[zbus(property, name = "MainPID")]
    fn main_pid(&self) -> zbus::Result<u32>;
}

/// Talks to systemd over D-Bus instead of running `systemctl`. The polkit rule in
/// `deploy/polkit` allows exactly start/stop/restart of this one unit.
pub struct SystemdUnitManager {
    bus: zbus::Connection,
    unit: String,
    journalctl: PathBuf,
}

impl SystemdUnitManager {
    pub fn new(bus: zbus::Connection, unit: String, journalctl: PathBuf) -> SystemdUnitManager {
        SystemdUnitManager { bus, unit, journalctl }
    }

    pub async fn restart(&self) -> Result<(), ServiceError> {
        let manager: SystemdManagerProxy<'_> = SystemdManagerProxy::new(&self.bus).await?;
        // "replace": a pending start/stop job for the unit is replaced by this one.
        // The call returns once the job is queued; the caller waits for mpv's socket.
        let _job: OwnedObjectPath = manager.restart_unit(&self.unit, "replace").await?;
        Ok(())
    }

    pub async fn status(&self) -> Result<ServiceStatus, ServiceError> {
        let manager: SystemdManagerProxy<'_> = SystemdManagerProxy::new(&self.bus).await?;
        let unit_path: OwnedObjectPath = manager.load_unit(&self.unit).await?;
        // Properties are fetched fresh on each request instead of being cached and
        // updated through signals, which this short-lived proxy would never see.
        let unit: SystemdUnitProxy<'_> = SystemdUnitProxy::builder(&self.bus)
            .path(unit_path.clone())?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let service: SystemdServiceProxy<'_> = SystemdServiceProxy::builder(&self.bus)
            .path(unit_path)?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let main_pid: u32 = service.main_pid().await?;
        Ok(ServiceStatus {
            manager: "systemd",
            unit: Some(self.unit.clone()),
            active_state: unit.active_state().await?,
            sub_state: unit.sub_state().await?,
            autostart: Some(unit.unit_file_state().await?),
            main_pid: (main_pid != 0).then_some(main_pid),
        })
    }

    /// Reading the system journal requires membership in the `systemd-journal` group.
    pub async fn recent_log(&self, lines: usize) -> Result<Vec<String>, ServiceError> {
        let output: std::process::Output = tokio::process::Command::new(&self.journalctl)
            .args([
                "--unit",
                &self.unit,
                "--lines",
                &lines.to_string(),
                "--output",
                "short-iso",
                "--no-pager",
                "--quiet",
            ])
            .output()
            .await
            .map_err(|source: std::io::Error| ServiceError::Spawn {
                command: self.journalctl.display().to_string(),
                source,
            })?;
        let text: String = String::from_utf8_lossy(&output.stdout).to_string();
        let mut collected: Vec<String> = text.lines().map(str::to_string).collect();
        if !output.status.success() {
            collected.push(format!("journalctl failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
        }
        Ok(collected)
    }
}
