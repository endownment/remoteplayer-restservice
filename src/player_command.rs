//! Typed player commands accepted by `POST /api/v1/player/command`.
//!
//! In OOP this would be a Command class hierarchy; in Rust an enum whose
//! variants carry their own data is the idiomatic sum type, and `match`
//! forces every variant to be handled.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::mpv::TrackSelection;
use crate::storage::{EntryName, MediaLibrary, StorageError};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PlayerCommand {
    #[serde(rename_all = "kebab-case")]
    Play {
        folder: String,
        name: String,
        #[serde(default)]
        mode: LoadMode,
    },
    #[serde(rename_all = "kebab-case")]
    PlayUrl {
        url: String,
        #[serde(default)]
        mode: LoadMode,
    },
    Pause,
    Resume,
    TogglePause,
    Stop,
    #[serde(rename_all = "kebab-case")]
    Seek {
        seconds: f64,
        #[serde(default)]
        mode: SeekMode,
    },
    SetVolume {
        volume: f64,
    },
    SetMute {
        muted: bool,
    },
    SetSpeed {
        speed: f64,
    },
    PlaylistNext,
    PlaylistPrevious,
    PlaylistPlay {
        index: usize,
    },
    PlaylistRemove {
        index: usize,
    },
    PlaylistMove {
        from: usize,
        to: usize,
    },
    PlaylistClear,
    SetAudioTrack {
        track: TrackSelection,
    },
    SetSubtitleTrack {
        track: TrackSelection,
    },
    AddSubtitle {
        folder: String,
        name: String,
    },
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoadMode {
    #[default]
    Replace,
    Append,
    AppendPlay,
}

impl LoadMode {
    fn mpv_flag(self) -> &'static str {
        match self {
            LoadMode::Replace => "replace",
            LoadMode::Append => "append",
            LoadMode::AppendPlay => "append-play",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeekMode {
    #[default]
    Relative,
    Absolute,
    AbsolutePercent,
    RelativePercent,
}

impl SeekMode {
    fn mpv_flag(self) -> &'static str {
        match self {
            SeekMode::Relative => "relative",
            SeekMode::Absolute => "absolute",
            SeekMode::AbsolutePercent => "absolute-percent",
            SeekMode::RelativePercent => "relative-percent",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("{0}")]
    Invalid(String),
}

const ALLOWED_URL_SCHEMES: [&str; 8] = ["http", "https", "rtsp", "rtmp", "rtmps", "srt", "udp", "smb"];

impl PlayerCommand {
    pub async fn to_mpv_command(&self, library: &MediaLibrary) -> Result<Vec<Value>, CommandError> {
        let command: Vec<Value> = match self {
            PlayerCommand::Play { folder, name, mode } => {
                let path: PathBuf = resolve(library, folder, name).await?;
                vec![json!("loadfile"), json!(path), json!(mode.mpv_flag())]
            }
            PlayerCommand::PlayUrl { url, mode } => {
                validate_url(url)?;
                vec![json!("loadfile"), json!(url), json!(mode.mpv_flag())]
            }
            PlayerCommand::Pause => vec![json!("set_property"), json!("pause"), json!(true)],
            PlayerCommand::Resume => vec![json!("set_property"), json!("pause"), json!(false)],
            PlayerCommand::TogglePause => vec![json!("cycle"), json!("pause")],
            PlayerCommand::Stop => vec![json!("stop")],
            PlayerCommand::Seek { seconds, mode } => {
                require_finite(*seconds, "seconds")?;
                vec![json!("seek"), json!(seconds), json!(mode.mpv_flag())]
            }
            PlayerCommand::SetVolume { volume } => {
                require_range(*volume, 0.0, 130.0, "volume")?;
                vec![json!("set_property"), json!("volume"), json!(volume)]
            }
            PlayerCommand::SetMute { muted } => vec![json!("set_property"), json!("mute"), json!(muted)],
            PlayerCommand::SetSpeed { speed } => {
                require_range(*speed, 0.01, 100.0, "speed")?;
                vec![json!("set_property"), json!("speed"), json!(speed)]
            }
            PlayerCommand::PlaylistNext => vec![json!("playlist-next")],
            PlayerCommand::PlaylistPrevious => vec![json!("playlist-prev")],
            PlayerCommand::PlaylistPlay { index } => vec![json!("playlist-play-index"), json!(index)],
            PlayerCommand::PlaylistRemove { index } => vec![json!("playlist-remove"), json!(index)],
            PlayerCommand::PlaylistMove { from, to } => vec![json!("playlist-move"), json!(from), json!(to)],
            PlayerCommand::PlaylistClear => vec![json!("playlist-clear")],
            PlayerCommand::SetAudioTrack { track } => {
                vec![json!("set_property"), json!("aid"), json!(track.to_mpv_option_value())]
            }
            PlayerCommand::SetSubtitleTrack { track } => {
                vec![json!("set_property"), json!("sid"), json!(track.to_mpv_option_value())]
            }
            PlayerCommand::AddSubtitle { folder, name } => {
                let path: PathBuf = resolve(library, folder, name).await?;
                vec![json!("sub-add"), json!(path), json!("select")]
            }
        };
        Ok(command)
    }
}

async fn resolve(library: &MediaLibrary, folder: &str, name: &str) -> Result<PathBuf, CommandError> {
    let folder: EntryName = EntryName::parse(folder).map_err(StorageError::from)?;
    let name: EntryName = EntryName::parse(name).map_err(StorageError::from)?;
    Ok(library.existing_file_path(&folder, &name).await?)
}

fn validate_url(url: &str) -> Result<(), CommandError> {
    let scheme: &str = url.split_once("://").map(|(scheme, _): (&str, &str)| scheme).unwrap_or_default();
    if ALLOWED_URL_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) {
        Ok(())
    } else {
        Err(CommandError::Invalid(format!("URL scheme must be one of {ALLOWED_URL_SCHEMES:?}")))
    }
}

fn require_finite(value: f64, field: &str) -> Result<(), CommandError> {
    if value.is_finite() { Ok(()) } else { Err(CommandError::Invalid(format!("{field} must be a finite number"))) }
}

fn require_range(value: f64, minimum: f64, maximum: f64, field: &str) -> Result<(), CommandError> {
    require_finite(value, field)?;
    if (minimum..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(CommandError::Invalid(format!("{field} must be between {minimum} and {maximum}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StorageConfig;

    #[test]
    fn commands_are_parsed_from_tagged_json() {
        let seek: PlayerCommand = serde_json::from_str(r#"{"type": "seek", "seconds": -10}"#).expect("parse");
        assert!(matches!(seek, PlayerCommand::Seek { seconds, mode: SeekMode::Relative } if seconds == -10.0));
        let track: PlayerCommand =
            serde_json::from_str(r#"{"type": "set-subtitle-track", "track": "no"}"#).expect("parse");
        assert!(matches!(track, PlayerCommand::SetSubtitleTrack { .. }));
        let unknown: Result<PlayerCommand, serde_json::Error> = serde_json::from_str(r#"{"type": "explode"}"#);
        assert!(unknown.is_err());
    }

    #[tokio::test]
    async fn play_resolves_library_paths_and_rejects_missing_files() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let library: MediaLibrary = MediaLibrary::open(&StorageConfig {
            upload_root: Some(directory.path().to_path_buf()),
            default_folder: "inbox".to_string(),
            min_free_bytes: 0,
            max_upload_bytes: None,
            allow_volatile: true,
        })
        .expect("library");
        std::fs::write(directory.path().join("inbox/a.mkv"), b"x").expect("seed");

        let play: PlayerCommand =
            PlayerCommand::Play { folder: "inbox".to_string(), name: "a.mkv".to_string(), mode: LoadMode::Append };
        let command: Vec<Value> = play.to_mpv_command(&library).await.expect("command");
        assert_eq!(command[0], "loadfile");
        assert!(command[1].as_str().expect("path").ends_with("/inbox/a.mkv"));
        assert_eq!(command[2], "append");

        let missing: PlayerCommand =
            PlayerCommand::Play { folder: "inbox".to_string(), name: "b.mkv".to_string(), mode: LoadMode::Replace };
        assert!(missing.to_mpv_command(&library).await.is_err());
    }

    #[test]
    fn only_network_url_schemes_are_accepted() {
        assert!(validate_url("https://example.org/video.mp4").is_ok());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("/etc/passwd").is_err());
    }
}
