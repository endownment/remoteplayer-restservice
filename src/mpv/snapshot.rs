//! Everything needed to bring mpv back to where it was after a restart.
//!
//! The same JSON layout is written by remoteplayer-mpvwatcher (`player` field
//! of its state file), so this service can restore from that file after a crash.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::mpv::client::{MpvClient, MpvError};

pub const SUPPORTED_STATE_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PlayerSnapshot {
    pub playlist: Vec<PlaylistItem>,
    pub playlist_pos: Option<usize>,
    pub time_pos: Option<f64>,
    pub paused: bool,
    pub volume: f64,
    pub muted: bool,
    pub speed: f64,
    pub audio_track: TrackSelection,
    pub subtitle_track: TrackSelection,
    pub secondary_subtitle_track: TrackSelection,
    pub audio_delay: f64,
    pub subtitle_delay: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PlaylistItem {
    pub filename: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// A track choice as mpv understands it: a track id, `no` (off) or `auto`.
/// `untagged` makes serde accept a bare number or one of the two keywords.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TrackSelection {
    Id(u32),
    Keyword(TrackKeyword),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKeyword {
    No,
    Auto,
}

impl TrackSelection {
    /// mpv reports `aid`/`sid` as a number, or as `false`/"no" when disabled.
    pub fn from_mpv_value(value: &Value) -> TrackSelection {
        match value {
            Value::Number(number) => number
                .as_u64()
                .and_then(|id: u64| u32::try_from(id).ok())
                .map(TrackSelection::Id)
                .unwrap_or(TrackSelection::Keyword(TrackKeyword::Auto)),
            Value::Bool(false) => TrackSelection::Keyword(TrackKeyword::No),
            Value::String(text) if text == "no" => TrackSelection::Keyword(TrackKeyword::No),
            _ => TrackSelection::Keyword(TrackKeyword::Auto),
        }
    }

    pub fn to_mpv_option_value(&self) -> String {
        match self {
            TrackSelection::Id(id) => id.to_string(),
            TrackSelection::Keyword(TrackKeyword::No) => "no".to_string(),
            TrackSelection::Keyword(TrackKeyword::Auto) => "auto".to_string(),
        }
    }
}

/// The state file of remoteplayer-mpvwatcher.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SavedPlayerState {
    pub format_version: u32,
    pub captured_at_unix_ms: u64,
    pub sequence: u64,
    pub mpv: MpvProcessInfo,
    pub player: PlayerSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MpvProcessInfo {
    pub pid: Option<u32>,
    pub version: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SavedStateError {
    #[error("cannot read {path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("cannot parse {path}: {source}")]
    Parse { path: String, source: serde_json::Error },
    #[error("{path} has format version {found}, this service understands {SUPPORTED_STATE_FORMAT_VERSION}")]
    UnsupportedVersion { path: String, found: u32 },
}

pub fn read_saved_state(path: &Path) -> Result<SavedPlayerState, SavedStateError> {
    let display: String = path.display().to_string();
    let text: String = std::fs::read_to_string(path)
        .map_err(|source: std::io::Error| SavedStateError::Io { path: display.clone(), source })?;
    let state: SavedPlayerState = serde_json::from_str(&text)
        .map_err(|source: serde_json::Error| SavedStateError::Parse { path: display.clone(), source })?;
    if state.format_version != SUPPORTED_STATE_FORMAT_VERSION {
        return Err(SavedStateError::UnsupportedVersion { path: display, found: state.format_version });
    }
    Ok(state)
}

#[derive(Debug, Deserialize)]
struct MpvPlaylistEntry {
    filename: String,
    #[serde(default)]
    title: Option<String>,
}

impl PlayerSnapshot {
    pub fn is_empty(&self) -> bool {
        self.playlist.is_empty()
    }

    pub async fn capture(client: &MpvClient) -> Result<PlayerSnapshot, MpvError> {
        let entries: Vec<MpvPlaylistEntry> = client.get_property_optional("playlist").await?.unwrap_or_default();
        let playlist_pos: Option<i64> = client.get_property_optional("playlist-pos").await?;
        Ok(PlayerSnapshot {
            playlist: entries
                .into_iter()
                .map(|entry: MpvPlaylistEntry| PlaylistItem { filename: entry.filename, title: entry.title })
                .collect(),
            playlist_pos: playlist_pos.and_then(|position: i64| usize::try_from(position).ok()),
            time_pos: client.get_property_optional("time-pos").await?,
            paused: client.get_property_optional("pause").await?.unwrap_or(false),
            volume: client.get_property_optional("volume").await?.unwrap_or(100.0),
            muted: client.get_property_optional("mute").await?.unwrap_or(false),
            speed: client.get_property_optional("speed").await?.unwrap_or(1.0),
            audio_track: read_audio_track(client).await?,
            subtitle_track: read_track(client, "sid").await?,
            secondary_subtitle_track: read_track(client, "secondary-sid").await?,
            audio_delay: client.get_property_optional("audio-delay").await?.unwrap_or(0.0),
            subtitle_delay: client.get_property_optional("sub-delay").await?.unwrap_or(0.0),
        })
    }

    /// The command sequence, kept free of I/O so it can be unit-tested.
    ///
    /// Uses the `loadfile <url> <flags> <index> <options>` form introduced in
    /// mpv 0.38. Every entry is appended without starting playback; selecting
    /// `playlist-pos` afterwards starts the right file, and the per-file
    /// options (`start=`, `aid=`...) given to that entry take effect then.
    pub fn restore_commands(&self) -> Vec<Vec<Value>> {
        let mut commands: Vec<Vec<Value>> = vec![
            vec![json!("stop")],
            vec![json!("set_property"), json!("pause"), json!(self.paused)],
            vec![json!("set_property"), json!("volume"), json!(self.volume)],
            vec![json!("set_property"), json!("mute"), json!(self.muted)],
            vec![json!("set_property"), json!("speed"), json!(self.speed)],
            vec![json!("set_property"), json!("audio-delay"), json!(self.audio_delay)],
            vec![json!("set_property"), json!("sub-delay"), json!(self.subtitle_delay)],
        ];
        for (index, item) in self.playlist.iter().enumerate() {
            let options: String =
                if Some(index) == self.playlist_pos { self.current_entry_options() } else { String::new() };
            commands.push(vec![json!("loadfile"), json!(item.filename), json!("append"), json!(-1), json!(options)]);
        }
        if let Some(position) = self.playlist_pos {
            if position < self.playlist.len() {
                commands.push(vec![json!("set_property"), json!("playlist-pos"), json!(position)]);
            }
        }
        commands
    }

    fn current_entry_options(&self) -> String {
        let mut options: Vec<String> = Vec::new();
        if let Some(seconds) = self.time_pos {
            options.push(format!("start={seconds:.3}"));
        }
        options.push(format!("aid={}", self.audio_track.to_mpv_option_value()));
        options.push(format!("sid={}", self.subtitle_track.to_mpv_option_value()));
        options.push(format!("secondary-sid={}", self.secondary_subtitle_track.to_mpv_option_value()));
        options.join(",")
    }

    /// Runs every command even if some fail; failures come back as warnings.
    pub async fn restore(&self, client: &MpvClient) -> Vec<String> {
        let mut warnings: Vec<String> = Vec::new();
        for command in self.restore_commands() {
            if let Err(error) = client.command(command.clone()).await {
                warnings.push(format!("{}: {error}", Value::Array(command)));
            }
        }
        warnings
    }
}

/// When mpv cannot open the audio output it deselects the audio track, so `aid`
/// reads "no" although nobody chose silence. Restoring that would keep the sound
/// off even after the output works again, so without an output it becomes `auto`.
async fn read_audio_track(client: &MpvClient) -> Result<TrackSelection, MpvError> {
    let selection: TrackSelection = read_track(client, "aid").await?;
    let audio_output: Option<String> = client.get_property_optional("current-ao").await?;
    match (&selection, audio_output) {
        (TrackSelection::Keyword(TrackKeyword::No), None) => Ok(TrackSelection::Keyword(TrackKeyword::Auto)),
        _ => Ok(selection),
    }
}

async fn read_track(client: &MpvClient, property: &str) -> Result<TrackSelection, MpvError> {
    let value: Option<Value> = client.get_property_optional(property).await?;
    Ok(value
        .map(|raw: Value| TrackSelection::from_mpv_value(&raw))
        .unwrap_or(TrackSelection::Keyword(TrackKeyword::Auto)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> PlayerSnapshot {
        PlayerSnapshot {
            playlist: vec![
                PlaylistItem { filename: "/media/a.mkv".to_string(), title: None },
                PlaylistItem { filename: "/media/b.mkv".to_string(), title: Some("B".to_string()) },
            ],
            playlist_pos: Some(1),
            time_pos: Some(83.25),
            paused: true,
            volume: 70.0,
            muted: false,
            speed: 1.0,
            audio_track: TrackSelection::Id(2),
            subtitle_track: TrackSelection::Keyword(TrackKeyword::No),
            secondary_subtitle_track: TrackSelection::Keyword(TrackKeyword::No),
            audio_delay: 0.0,
            subtitle_delay: 0.0,
        }
    }

    #[test]
    fn only_the_current_entry_gets_start_and_track_options() {
        let commands: Vec<Vec<Value>> = sample_snapshot().restore_commands();
        let loads: Vec<&Vec<Value>> =
            commands.iter().filter(|command: &&Vec<Value>| command[0] == "loadfile").collect();
        assert_eq!(loads[0], &vec![json!("loadfile"), json!("/media/a.mkv"), json!("append"), json!(-1), json!("")]);
        assert_eq!(
            loads[1],
            &vec![
                json!("loadfile"),
                json!("/media/b.mkv"),
                json!("append"),
                json!(-1),
                json!("start=83.250,aid=2,sid=no,secondary-sid=no")
            ]
        );
        assert_eq!(commands.last().expect("last"), &vec![json!("set_property"), json!("playlist-pos"), json!(1)]);
    }

    #[test]
    fn pause_is_set_before_any_file_is_loaded() {
        let commands: Vec<Vec<Value>> = sample_snapshot().restore_commands();
        let pause_index: usize =
            commands.iter().position(|command: &Vec<Value>| command.get(1) == Some(&json!("pause"))).expect("pause");
        let first_load: usize =
            commands.iter().position(|command: &Vec<Value>| command[0] == "loadfile").expect("load");
        assert!(pause_index < first_load);
    }

    #[test]
    fn track_selection_reads_mpv_representations() {
        assert_eq!(TrackSelection::from_mpv_value(&json!(3)), TrackSelection::Id(3));
        assert_eq!(TrackSelection::from_mpv_value(&json!(false)), TrackSelection::Keyword(TrackKeyword::No));
        assert_eq!(TrackSelection::from_mpv_value(&json!("no")), TrackSelection::Keyword(TrackKeyword::No));
        assert_eq!(TrackSelection::from_mpv_value(&json!("auto")), TrackSelection::Keyword(TrackKeyword::Auto));
    }

    #[test]
    fn snapshot_json_round_trips_with_kebab_case_keys() {
        let text: String = serde_json::to_string(&sample_snapshot()).expect("encode");
        assert!(text.contains("\"playlist-pos\":1"));
        assert!(text.contains("\"audio-track\":2"));
        assert!(text.contains("\"subtitle-track\":\"no\""));
        let decoded: PlayerSnapshot = serde_json::from_str(&text).expect("decode");
        assert_eq!(decoded, sample_snapshot());
    }
}
