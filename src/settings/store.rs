use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use serde::Serialize;

use crate::fs_util::{describe_io_error, is_volatile, probe_writable, read_optional_to_string, write_atomically};
use crate::settings::{PlayerSettings, SettingsPatch, ValidationError};

pub const ARGS_VARIABLE_NAME: &str = "REMOTEPLAYER_MPV_ARGS";

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(transparent)]
    Invalid(#[from] ValidationError),
    #[error("cannot access {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("cannot encode settings: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Whether changed settings survive a reboot, probed when asked.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SettingsPersistence {
    /// Both files can be written; otherwise every change is refused.
    pub writable: bool,
    /// Written, but to RAM (tmpfs/overlay): lost on power-off.
    pub volatile: bool,
    pub reason: Option<String>,
}

/// Owns the persisted player settings and the systemd EnvironmentFile derived from them.
pub struct SettingsStore {
    settings_file: PathBuf,
    args_env_file: PathBuf,
    defaults: PlayerSettings,
    // std (not tokio) RwLock: it is never held across an `.await`, so blocking
    // for a few nanoseconds is cheaper than an async lock.
    current: RwLock<PlayerSettings>,
}

impl SettingsStore {
    pub fn load(
        settings_file: PathBuf,
        args_env_file: PathBuf,
        defaults: PlayerSettings,
    ) -> Result<SettingsStore, SettingsError> {
        defaults.validate()?;
        let stored_text: Option<String> = read_optional_to_string(&settings_file)
            .map_err(|source: std::io::Error| SettingsError::Io { path: settings_file.clone(), source })?;
        let current: PlayerSettings = match stored_text {
            None => defaults.clone(),
            // Stored settings are read as a patch over the defaults, so a setting
            // added in a newer version simply gets its default value.
            Some(text) => match serde_json::from_str::<SettingsPatch>(&text) {
                Ok(patch) => defaults.with_patch(&patch, &defaults),
                Err(error) => {
                    tracing::warn!(file = %settings_file.display(), %error, "stored settings are unreadable, using defaults");
                    defaults.clone()
                }
            },
        };
        current.validate()?;
        Ok(SettingsStore { settings_file, args_env_file, defaults, current: RwLock::new(current) })
    }

    pub fn current(&self) -> PlayerSettings {
        // A poisoned lock only means another thread panicked while holding it;
        // the PlayerSettings value inside is still a complete value, so keep using it.
        self.current.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn defaults(&self) -> &PlayerSettings {
        &self.defaults
    }

    pub fn persist(&self, settings: &PlayerSettings) -> Result<(), SettingsError> {
        settings.validate()?;
        let json: String = serde_json::to_string_pretty(settings)?;
        write_atomically(&self.settings_file, json.as_bytes())
            .map_err(|source: std::io::Error| SettingsError::Io { path: self.settings_file.clone(), source })?;
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = settings.clone();
        Ok(())
    }

    pub fn persistence(&self) -> SettingsPersistence {
        let mut volatile: bool = false;
        for file in [&self.settings_file, &self.args_env_file] {
            let directory: &Path = file.parent().unwrap_or(Path::new("."));
            if let Err(error) = probe_writable(directory) {
                let reason: String = format!("{}: {}", directory.display(), describe_io_error(&error));
                return SettingsPersistence { writable: false, volatile: false, reason: Some(reason) };
            }
            volatile |= is_volatile(directory);
        }
        SettingsPersistence { writable: true, volatile, reason: None }
    }

    pub fn args_env_file(&self) -> &Path {
        &self.args_env_file
    }

    pub fn read_env_file(&self) -> Result<Option<String>, SettingsError> {
        read_optional_to_string(&self.args_env_file)
            .map_err(|source: std::io::Error| SettingsError::Io { path: self.args_env_file.clone(), source })
    }

    pub fn write_env_file(&self, settings: &PlayerSettings) -> Result<(), SettingsError> {
        settings.validate()?;
        self.write_env_file_text(&render_env_file(settings))
    }

    pub fn write_env_file_text(&self, text: &str) -> Result<(), SettingsError> {
        write_atomically(&self.args_env_file, text.as_bytes())
            .map_err(|source: std::io::Error| SettingsError::Io { path: self.args_env_file.clone(), source })
    }

    /// Returns `true` when the file had to be rewritten.
    pub fn ensure_env_file_matches_current(&self) -> Result<bool, SettingsError> {
        let expected: String = render_env_file(&self.current());
        let actual: Option<String> = self.read_env_file()?;
        if actual.as_deref() == Some(expected.as_str()) {
            return Ok(false);
        }
        self.write_env_file_text(&expected)?;
        Ok(true)
    }
}

pub fn render_env_file(settings: &PlayerSettings) -> String {
    let args: String = settings.to_mpv_args().join(" ");
    format!(
        "# Generated by remoteplayer-restservice from its settings file. Manual edits are overwritten.\n\
         {ARGS_VARIABLE_NAME}='{args}'\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_holds_all_arguments_in_one_variable() {
        let text: String = render_env_file(&PlayerSettings::builtin_defaults());
        let assignment: &str = text.lines().nth(1).expect("assignment line");
        assert!(assignment.starts_with("REMOTEPLAYER_MPV_ARGS='--audio-device=alsa/hdmi:CARD=vc4hdmi,DEV=0 --vo=gpu"));
        assert!(assignment.ends_with("--demuxer-max-back-bytes=10MiB'"));
    }

    #[test]
    fn stored_settings_survive_reload_and_missing_keys_take_defaults() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let settings_file: PathBuf = directory.path().join("settings.json");
        let env_file: PathBuf = directory.path().join("mpv.env");
        std::fs::write(&settings_file, r#"{"vo": "drm"}"#).expect("seed file");

        let store: SettingsStore =
            SettingsStore::load(settings_file.clone(), env_file.clone(), PlayerSettings::builtin_defaults())
                .expect("load");
        assert_eq!(store.current().vo, "drm");
        assert_eq!(store.current().hwdec, "v4l2m2m");

        assert!(store.ensure_env_file_matches_current().expect("first sync"));
        assert!(!store.ensure_env_file_matches_current().expect("second sync"));

        let mut changed: PlayerSettings = store.current();
        changed.hwdec = "no".to_string();
        store.persist(&changed).expect("persist");
        let reloaded: SettingsStore =
            SettingsStore::load(settings_file, env_file, PlayerSettings::builtin_defaults()).expect("reload");
        assert_eq!(reloaded.current().hwdec, "no");
    }
}
