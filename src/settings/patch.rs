use serde::{Deserialize, Deserializer};

/// One field of a JSON Merge Patch (RFC 7396): a missing key keeps the current
/// value, `null` resets it to the default, anything else replaces it.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Patch<T> {
    #[default]
    Keep,
    Reset,
    Set(T),
}

impl<T: Clone> Patch<T> {
    pub fn resolve(&self, current: &T, default: &T) -> T {
        match self {
            Patch::Keep => current.clone(),
            Patch::Reset => default.clone(),
            Patch::Set(value) => value.clone(),
        }
    }
}

// serde never calls this for a key that is absent from the input: absent keys
// come from `#[serde(default)]` (= `Keep`), so here we only see `null` or a value.
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Patch<T>, D::Error> {
        let value: Option<T> = Option::<T>::deserialize(deserializer)?;
        match value {
            Some(inner) => Ok(Patch::Set(inner)),
            None => Ok(Patch::Reset),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct SettingsPatch {
    pub audio_device: Patch<String>,
    pub audio_spdif: Patch<String>,
    pub vo: Patch<String>,
    pub gpu_context: Patch<String>,
    pub hwdec: Patch<String>,
    pub drm_draw_plane: Patch<String>,
    pub drm_drmprime_video_plane: Patch<String>,
    pub drm_mode: Patch<String>,
    pub fullscreen: Patch<bool>,
    pub force_window: Patch<bool>,
    pub script_opts: Patch<String>,
    pub demuxer_max_bytes: Patch<String>,
    pub demuxer_max_back_bytes: Patch<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::PlayerSettings;

    #[test]
    fn missing_null_and_value_map_to_keep_reset_and_set() {
        let patch: SettingsPatch =
            serde_json::from_str(r#"{"vo": null, "audio-device": "alsa/plughw:CARD=Headphones"}"#).expect("parse");
        assert_eq!(patch.vo, Patch::Reset);
        assert_eq!(patch.audio_device, Patch::Set("alsa/plughw:CARD=Headphones".to_string()));
        assert_eq!(patch.hwdec, Patch::Keep);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let result: Result<SettingsPatch, serde_json::Error> = serde_json::from_str(r#"{"audio-devise": "x"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn patch_resolves_against_current_and_defaults() {
        let defaults: PlayerSettings = PlayerSettings::builtin_defaults();
        let mut current: PlayerSettings = defaults.clone();
        current.vo = "drm".to_string();
        current.hwdec = "no".to_string();
        let patch: SettingsPatch = serde_json::from_str(r#"{"vo": null, "fullscreen": false}"#).expect("parse");
        let result: PlayerSettings = current.with_patch(&patch, &defaults);
        assert_eq!(result.vo, "gpu");
        assert_eq!(result.hwdec, "no");
        assert!(!result.fullscreen);
    }
}
