mod patch;
mod store;

pub use patch::SettingsPatch;
pub use store::{SettingsError, SettingsPersistence, SettingsStore};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Every tunable mpv option the service manages. Keys in JSON and TOML are the
/// mpv option names, so `audio-device` here is `--audio-device` for mpv.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PlayerSettings {
    pub audio_device: String,
    pub audio_spdif: String,
    pub vo: String,
    pub gpu_context: String,
    pub hwdec: String,
    pub drm_draw_plane: String,
    pub drm_drmprime_video_plane: String,
    pub drm_mode: String,
    pub fullscreen: bool,
    pub force_window: bool,
    pub script_opts: String,
    pub demuxer_max_bytes: String,
    pub demuxer_max_back_bytes: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplyMode {
    /// mpv accepts the new value through `set_property` while it keeps playing.
    Runtime,
    /// mpv only reads the option at startup.
    Restart,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Text(String),
    Flag(bool),
}

impl SettingValue {
    pub fn to_json(&self) -> Value {
        match self {
            SettingValue::Text(text) => Value::String(text.clone()),
            SettingValue::Flag(flag) => Value::Bool(*flag),
        }
    }

    fn to_command_line_value(&self) -> Option<String> {
        match self {
            SettingValue::Text(text) if text.is_empty() => None,
            SettingValue::Text(text) => Some(text.clone()),
            SettingValue::Flag(true) => Some("yes".to_string()),
            SettingValue::Flag(false) => Some("no".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingEntry {
    pub key: &'static str,
    pub value: SettingValue,
    pub apply_mode: ApplyMode,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SettingChange {
    pub key: &'static str,
    pub from: Value,
    pub to: Value,
    pub apply_mode: ApplyMode,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid value for `{key}`: {reason}")]
pub struct ValidationError {
    pub key: &'static str,
    pub reason: String,
}

const MAX_TEXT_LENGTH: usize = 512;

impl PlayerSettings {
    /// The values of the hand-tested `remoteplayer-mpv.service` on a Raspberry Pi 3.
    pub fn builtin_defaults() -> PlayerSettings {
        PlayerSettings {
            audio_device: "alsa/hdmi:CARD=vc4hdmi,DEV=0".to_string(),
            audio_spdif: String::new(),
            vo: "gpu".to_string(),
            gpu_context: "drm".to_string(),
            hwdec: "v4l2m2m".to_string(),
            drm_draw_plane: "overlay".to_string(),
            drm_drmprime_video_plane: "primary".to_string(),
            drm_mode: String::new(),
            fullscreen: true,
            force_window: true,
            script_opts: "osc-idlescreen=no".to_string(),
            demuxer_max_bytes: "40MiB".to_string(),
            demuxer_max_back_bytes: "10MiB".to_string(),
        }
    }

    pub fn entries(&self) -> Vec<SettingEntry> {
        vec![
            text_entry("audio-device", &self.audio_device, ApplyMode::Runtime, true),
            text_entry("audio-spdif", &self.audio_spdif, ApplyMode::Runtime, false),
            text_entry("vo", &self.vo, ApplyMode::Restart, true),
            text_entry("gpu-context", &self.gpu_context, ApplyMode::Restart, false),
            text_entry("hwdec", &self.hwdec, ApplyMode::Runtime, true),
            text_entry("drm-draw-plane", &self.drm_draw_plane, ApplyMode::Restart, false),
            text_entry("drm-drmprime-video-plane", &self.drm_drmprime_video_plane, ApplyMode::Restart, false),
            text_entry("drm-mode", &self.drm_mode, ApplyMode::Restart, false),
            flag_entry("fullscreen", self.fullscreen, ApplyMode::Runtime),
            flag_entry("force-window", self.force_window, ApplyMode::Restart),
            text_entry("script-opts", &self.script_opts, ApplyMode::Restart, false),
            text_entry("demuxer-max-bytes", &self.demuxer_max_bytes, ApplyMode::Runtime, true),
            text_entry("demuxer-max-back-bytes", &self.demuxer_max_back_bytes, ApplyMode::Runtime, true),
        ]
    }

    pub fn apply_modes() -> Vec<(&'static str, ApplyMode)> {
        PlayerSettings::builtin_defaults()
            .entries()
            .into_iter()
            .map(|entry: SettingEntry| (entry.key, entry.apply_mode))
            .collect()
    }

    /// `Keep` takes the value from `self`, `Reset` from `defaults`.
    pub fn with_patch(&self, patch: &SettingsPatch, defaults: &PlayerSettings) -> PlayerSettings {
        PlayerSettings {
            audio_device: patch.audio_device.resolve(&self.audio_device, &defaults.audio_device),
            audio_spdif: patch.audio_spdif.resolve(&self.audio_spdif, &defaults.audio_spdif),
            vo: patch.vo.resolve(&self.vo, &defaults.vo),
            gpu_context: patch.gpu_context.resolve(&self.gpu_context, &defaults.gpu_context),
            hwdec: patch.hwdec.resolve(&self.hwdec, &defaults.hwdec),
            drm_draw_plane: patch.drm_draw_plane.resolve(&self.drm_draw_plane, &defaults.drm_draw_plane),
            drm_drmprime_video_plane: patch
                .drm_drmprime_video_plane
                .resolve(&self.drm_drmprime_video_plane, &defaults.drm_drmprime_video_plane),
            drm_mode: patch.drm_mode.resolve(&self.drm_mode, &defaults.drm_mode),
            fullscreen: patch.fullscreen.resolve(&self.fullscreen, &defaults.fullscreen),
            force_window: patch.force_window.resolve(&self.force_window, &defaults.force_window),
            script_opts: patch.script_opts.resolve(&self.script_opts, &defaults.script_opts),
            demuxer_max_bytes: patch.demuxer_max_bytes.resolve(&self.demuxer_max_bytes, &defaults.demuxer_max_bytes),
            demuxer_max_back_bytes: patch
                .demuxer_max_back_bytes
                .resolve(&self.demuxer_max_back_bytes, &defaults.demuxer_max_back_bytes),
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        for entry in self.entries() {
            if let SettingValue::Text(text) = &entry.value {
                validate_text(entry.key, text, entry.required)?;
            }
        }
        Ok(())
    }

    pub fn changes_to(&self, next: &PlayerSettings) -> Vec<SettingChange> {
        let before: Vec<SettingEntry> = self.entries();
        let after: Vec<SettingEntry> = next.entries();
        before
            .into_iter()
            .zip(after)
            .filter(|(old, new): &(SettingEntry, SettingEntry)| old.value != new.value)
            .map(|(old, new): (SettingEntry, SettingEntry)| SettingChange {
                key: old.key,
                from: old.value.to_json(),
                to: new.value.to_json(),
                apply_mode: old.apply_mode,
            })
            .collect()
    }

    /// Empty text values are omitted, so mpv keeps its own default for them.
    pub fn to_mpv_args(&self) -> Vec<String> {
        self.entries()
            .into_iter()
            .filter_map(|entry: SettingEntry| {
                entry.value.to_command_line_value().map(|value: String| format!("--{}={}", entry.key, value))
            })
            .collect()
    }
}

fn text_entry(key: &'static str, value: &str, apply_mode: ApplyMode, required: bool) -> SettingEntry {
    SettingEntry { key, value: SettingValue::Text(value.to_string()), apply_mode, required }
}

fn flag_entry(key: &'static str, value: bool, apply_mode: ApplyMode) -> SettingEntry {
    SettingEntry { key, value: SettingValue::Flag(value), apply_mode, required: true }
}

/// The arguments end up in a systemd EnvironmentFile and are split on whitespace
/// by `ExecStart=... $REMOTEPLAYER_MPV_ARGS`, so whitespace and quoting characters
/// would let a value smuggle in extra mpv arguments.
fn validate_text(key: &'static str, value: &str, required: bool) -> Result<(), ValidationError> {
    if required && value.is_empty() {
        return Err(ValidationError { key, reason: "must not be empty".to_string() });
    }
    if value.len() > MAX_TEXT_LENGTH {
        return Err(ValidationError { key, reason: format!("must be at most {MAX_TEXT_LENGTH} bytes") });
    }
    let forbidden: Option<char> = value
        .chars()
        .find(|character: &char| character.is_whitespace() || character.is_control() || "'\"\\$`".contains(*character));
    if let Some(character) = forbidden {
        return Err(ValidationError { key, reason: format!("character {character:?} is not allowed") });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_defaults_render_the_reference_command_line() {
        let args: Vec<String> = PlayerSettings::builtin_defaults().to_mpv_args();
        assert_eq!(
            args,
            vec![
                "--audio-device=alsa/hdmi:CARD=vc4hdmi,DEV=0",
                "--vo=gpu",
                "--gpu-context=drm",
                "--hwdec=v4l2m2m",
                "--drm-draw-plane=overlay",
                "--drm-drmprime-video-plane=primary",
                "--fullscreen=yes",
                "--force-window=yes",
                "--script-opts=osc-idlescreen=no",
                "--demuxer-max-bytes=40MiB",
                "--demuxer-max-back-bytes=10MiB",
            ]
        );
    }

    #[test]
    fn builtin_defaults_are_valid() {
        assert_eq!(PlayerSettings::builtin_defaults().validate(), Ok(()));
    }

    #[test]
    fn whitespace_cannot_inject_extra_arguments() {
        let mut settings: PlayerSettings = PlayerSettings::builtin_defaults();
        settings.audio_device = "alsa/default --script=/tmp/evil.lua".to_string();
        let error: ValidationError = settings.validate().expect_err("must be rejected");
        assert_eq!(error.key, "audio-device");
    }

    #[test]
    fn required_values_cannot_be_empty() {
        let mut settings: PlayerSettings = PlayerSettings::builtin_defaults();
        settings.vo = String::new();
        assert_eq!(settings.validate().expect_err("must be rejected").key, "vo");
    }

    #[test]
    fn changes_report_only_modified_keys_with_their_apply_mode() {
        let before: PlayerSettings = PlayerSettings::builtin_defaults();
        let mut after: PlayerSettings = before.clone();
        after.audio_device = "alsa/plughw:CARD=Headphones".to_string();
        after.vo = "drm".to_string();
        let changes: Vec<SettingChange> = before.changes_to(&after);
        let summary: Vec<(&str, ApplyMode)> =
            changes.iter().map(|change: &SettingChange| (change.key, change.apply_mode)).collect();
        assert_eq!(summary, vec![("audio-device", ApplyMode::Runtime), ("vo", ApplyMode::Restart)]);
    }
}
