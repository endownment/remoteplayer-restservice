use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mpv::client::{MpvClient, MpvError};

/// Properties the event stream watches; changes are pushed to WebSocket clients.
pub const OBSERVED_PROPERTIES: [&str; 15] = [
    "pause",
    "idle-active",
    "path",
    "media-title",
    "duration",
    "time-pos",
    "volume",
    "mute",
    "speed",
    "playlist-pos",
    "playlist-count",
    "aid",
    "sid",
    "audio-device",
    "eof-reached",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct PlayerState {
    pub connected: bool,
    pub pid: Option<u32>,
    pub mpv_version: Option<String>,
    pub idle: bool,
    pub paused: bool,
    pub path: Option<String>,
    pub media_title: Option<String>,
    pub time_pos: Option<f64>,
    pub duration: Option<f64>,
    pub percent_pos: Option<f64>,
    pub volume: Option<f64>,
    pub muted: bool,
    pub speed: Option<f64>,
    pub playlist_pos: Option<i64>,
    pub playlist: Vec<PlaylistEntry>,
    pub tracks: Vec<TrackInfo>,
    pub audio_device: Option<String>,
    pub audio_output: Option<String>,
    pub hwdec_current: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PlaylistEntry {
    pub filename: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub current: bool,
    #[serde(default)]
    pub playing: bool,
}

/// One element of mpv's `track-list`; mpv sends more fields, serde ignores them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TrackInfo {
    pub id: u32,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub external: bool,
}

impl PlayerState {
    pub fn disconnected() -> PlayerState {
        PlayerState {
            connected: false,
            pid: None,
            mpv_version: None,
            idle: true,
            paused: false,
            path: None,
            media_title: None,
            time_pos: None,
            duration: None,
            percent_pos: None,
            volume: None,
            muted: false,
            speed: None,
            playlist_pos: None,
            playlist: Vec::new(),
            tracks: Vec::new(),
            audio_device: None,
            audio_output: None,
            hwdec_current: None,
        }
    }

    pub async fn query(client: &MpvClient) -> Result<PlayerState, MpvError> {
        if !client.is_connected() {
            return Ok(PlayerState::disconnected());
        }
        let playlist_pos: Option<i64> =
            client.get_property_optional::<i64>("playlist-pos").await?.filter(|position: &i64| *position >= 0);
        Ok(PlayerState {
            connected: true,
            pid: client.get_property_optional("pid").await?,
            mpv_version: client.get_property_optional("mpv-version").await?,
            idle: client.get_property_optional("idle-active").await?.unwrap_or(true),
            paused: client.get_property_optional("pause").await?.unwrap_or(false),
            path: client.get_property_optional("path").await?,
            media_title: client.get_property_optional("media-title").await?,
            time_pos: client.get_property_optional("time-pos").await?,
            duration: client.get_property_optional("duration").await?,
            percent_pos: client.get_property_optional("percent-pos").await?,
            volume: client.get_property_optional("volume").await?,
            muted: client.get_property_optional("mute").await?.unwrap_or(false),
            speed: client.get_property_optional("speed").await?,
            playlist_pos,
            playlist: client.get_property_optional("playlist").await?.unwrap_or_default(),
            tracks: client.get_property_optional("track-list").await?.unwrap_or_default(),
            audio_device: client.get_property_optional("audio-device").await?,
            audio_output: client.get_property_optional("current-ao").await?,
            hwdec_current: client.get_property_optional::<Value>("hwdec-current").await?.and_then(text_value),
        })
    }
}

fn text_value(value: Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text),
        _ => None,
    }
}
