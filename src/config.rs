use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::settings::SettingsPatch;

const MIN_TOKEN_LENGTH: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config file {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("invalid config file {path}: {source}")]
    Parse { path: PathBuf, source: toml::de::Error },
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AppConfig {
    pub listen: SocketAddr,
    pub auth: AuthConfig,
    pub storage: StorageConfig,
    pub mpv: MpvConfig,
    pub service_manager: ServiceManagerConfig,
    #[serde(default)]
    pub cec: CecConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub system: SystemConfig,
    #[serde(default)]
    pub restore: RestoreConfig,
    #[serde(default)]
    pub player_defaults: SettingsPatch,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AuthConfig {
    pub token: Option<String>,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct StorageConfig {
    pub upload_root: PathBuf,
    pub default_folder: String,
    #[serde(default = "default_min_free_bytes")]
    pub min_free_bytes: u64,
    pub max_upload_bytes: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MpvConfig {
    pub ipc_socket: PathBuf,
    pub settings_file: PathBuf,
    pub args_env_file: PathBuf,
    #[serde(default = "default_mpv_executable")]
    pub executable: PathBuf,
    #[serde(default = "default_restart_timeout_seconds")]
    pub restart_timeout_seconds: u64,
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub raw_ipc_enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ServiceManagerConfig {
    #[serde(rename_all = "kebab-case")]
    Systemd {
        unit: String,
        #[serde(default = "default_journalctl")]
        journalctl: PathBuf,
    },
    #[serde(rename_all = "kebab-case")]
    ChildProcess {
        command: PathBuf,
        #[serde(default)]
        fixed_args: Vec<String>,
        #[serde(default = "default_respawn_delay_seconds")]
        respawn_delay_seconds: u64,
    },
}

impl ServiceManagerConfig {
    pub fn needs_system_bus(&self) -> bool {
        matches!(self, ServiceManagerConfig::Systemd { .. })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CecConfig {
    #[default]
    Disabled,
    Stub,
    #[serde(rename_all = "kebab-case")]
    Linux {
        device: PathBuf,
        #[serde(default = "default_true")]
        claim_logical_address: bool,
        #[serde(default = "default_osd_name")]
        osd_name: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DisplayConfig {
    #[serde(default = "default_drm_sysfs_root")]
    pub drm_sysfs_root: PathBuf,
    pub preferred_connector: Option<String>,
}

impl Default for DisplayConfig {
    fn default() -> DisplayConfig {
        DisplayConfig { drm_sysfs_root: default_drm_sysfs_root(), preferred_connector: None }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SystemConfig {
    #[serde(default = "default_thermal_zone_file")]
    pub thermal_zone_file: PathBuf,
    #[serde(default = "default_throttled_file")]
    pub throttled_file: PathBuf,
    #[serde(default = "default_vcgencmd")]
    pub vcgencmd: PathBuf,
    #[serde(default)]
    pub power: PowerConfig,
}

impl Default for SystemConfig {
    fn default() -> SystemConfig {
        SystemConfig {
            thermal_zone_file: default_thermal_zone_file(),
            throttled_file: default_throttled_file(),
            vcgencmd: default_vcgencmd(),
            power: PowerConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PowerConfig {
    #[default]
    Disabled,
    Stub,
    Logind,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RestoreConfig {
    #[serde(default = "default_true")]
    pub after_crash: bool,
    pub watcher_state_file: Option<PathBuf>,
}

impl Default for RestoreConfig {
    fn default() -> RestoreConfig {
        RestoreConfig { after_crash: true, watcher_state_file: None }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<AppConfig, ConfigError> {
        let text: String = std::fs::read_to_string(path)
            .map_err(|source: std::io::Error| ConfigError::Read { path: path.to_path_buf(), source })?;
        let config: AppConfig = toml::from_str(&text)
            .map_err(|source: toml::de::Error| ConfigError::Parse { path: path.to_path_buf(), source })?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let token_length: usize = self.auth.token.as_deref().map(str::len).unwrap_or(0);
        if !self.auth.disabled && token_length < MIN_TOKEN_LENGTH {
            return Err(ConfigError::Invalid(format!(
                "auth.token must be at least {MIN_TOKEN_LENGTH} characters, e.g. `openssl rand -hex 24` \
                 (or set auth.disabled = true to run without authentication)"
            )));
        }
        if self.mpv.restart_timeout_seconds == 0 || self.mpv.request_timeout_seconds == 0 {
            return Err(ConfigError::Invalid("mpv timeouts must be greater than zero".to_string()));
        }
        Ok(())
    }

    pub fn needs_system_bus(&self) -> bool {
        self.service_manager.needs_system_bus() || self.system.power == PowerConfig::Logind
    }
}

fn default_true() -> bool {
    true
}

fn default_min_free_bytes() -> u64 {
    256 * 1024 * 1024
}

fn default_mpv_executable() -> PathBuf {
    PathBuf::from("/usr/bin/mpv")
}

fn default_restart_timeout_seconds() -> u64 {
    20
}

fn default_request_timeout_seconds() -> u64 {
    5
}

fn default_journalctl() -> PathBuf {
    PathBuf::from("/usr/bin/journalctl")
}

fn default_respawn_delay_seconds() -> u64 {
    3
}

fn default_osd_name() -> String {
    "remoteplayer".to_string()
}

fn default_drm_sysfs_root() -> PathBuf {
    PathBuf::from("/sys/class/drm")
}

fn default_thermal_zone_file() -> PathBuf {
    PathBuf::from("/sys/class/thermal/thermal_zone0/temp")
}

fn default_throttled_file() -> PathBuf {
    PathBuf::from("/sys/devices/platform/soc/soc:firmware/get_throttled")
}

fn default_vcgencmd() -> PathBuf {
    PathBuf::from("/usr/bin/vcgencmd")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_CONFIG: &str = r#"
        listen = "127.0.0.1:8080"

        [auth]
        token = "secret-token-0123456789"

        [storage]
        upload-root = "/tmp/uploads"
        default-folder = "inbox"

        [mpv]
        ipc-socket = "/tmp/mpv.sock"
        settings-file = "/tmp/settings.json"
        args-env-file = "/tmp/mpv.env"

        [service-manager]
        kind = "child-process"
        command = "mpv"
    "#;

    #[test]
    fn minimal_config_uses_defaults_for_optional_sections() {
        let config: AppConfig = toml::from_str(MINIMAL_CONFIG).expect("config must parse");
        config.validate().expect("config must be valid");
        assert!(matches!(config.cec, CecConfig::Disabled));
        assert_eq!(config.system.power, PowerConfig::Disabled);
        assert!(config.restore.after_crash);
        assert_eq!(config.mpv.restart_timeout_seconds, 20);
    }

    #[test]
    fn placeholder_token_is_too_short() {
        let placeholder: String = MINIMAL_CONFIG.replace("secret-token-0123456789", "CHANGE-ME");
        let config: AppConfig = toml::from_str(&placeholder).expect("config must parse");
        assert!(config.validate().is_err());
    }

    #[test]
    fn missing_token_is_rejected_unless_auth_is_disabled() {
        let without_token: String = MINIMAL_CONFIG.replace("token = \"secret-token-0123456789\"", "");
        let config: AppConfig = toml::from_str(&without_token).expect("config must parse");
        assert!(config.validate().is_err());

        let disabled: String = without_token.replace("[auth]", "[auth]\ndisabled = true");
        let config: AppConfig = toml::from_str(&disabled).expect("config must parse");
        assert!(config.validate().is_ok());
    }
}
