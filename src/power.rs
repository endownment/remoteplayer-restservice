use serde::Serialize;

use crate::config::PowerConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PowerAction {
    Reboot,
    PowerOff,
}

#[derive(Debug, thiserror::Error)]
pub enum PowerError {
    #[error("power control is disabled in the configuration")]
    Disabled,
    #[error("D-Bus call to logind failed: {0}")]
    DBus(#[from] zbus::Error),
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LoginManager {
    fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    fn power_off(&self, interactive: bool) -> zbus::Result<()>;
}

pub enum PowerControl {
    Disabled,
    Stub,
    Logind(zbus::Connection),
}

impl PowerControl {
    pub fn from_config(config: PowerConfig, system_bus: Option<zbus::Connection>) -> PowerControl {
        match (config, system_bus) {
            (PowerConfig::Logind, Some(bus)) => PowerControl::Logind(bus),
            (PowerConfig::Stub, _) => PowerControl::Stub,
            (PowerConfig::Disabled, _) | (PowerConfig::Logind, None) => PowerControl::Disabled,
        }
    }

    pub async fn perform(&self, action: PowerAction) -> Result<(), PowerError> {
        match self {
            PowerControl::Disabled => Err(PowerError::Disabled),
            PowerControl::Stub => {
                tracing::warn!(?action, "power stub: pretending to perform the action");
                Ok(())
            }
            PowerControl::Logind(bus) => {
                let manager: LoginManagerProxy<'_> = LoginManagerProxy::new(bus).await?;
                // interactive = false: never wait for a password prompt; polkit
                // either allows the call (see deploy/polkit) or refuses it.
                match action {
                    PowerAction::Reboot => manager.reboot(false).await?,
                    PowerAction::PowerOff => manager.power_off(false).await?,
                }
                Ok(())
            }
        }
    }
}
