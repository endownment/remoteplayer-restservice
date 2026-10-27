//! Client for the state socket of remoteplayer-mpvwatcher. Only `last` is
//! used: the snapshot the watcher keeps for restoring playback.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::mpv::{SUPPORTED_STATE_FORMAT_VERSION, SavedPlayerState};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const RETRY_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    #[error("cannot reach remoteplayer-mpvwatcher at {socket}: {source}")]
    Unreachable { socket: String, source: std::io::Error },
    #[error("remoteplayer-mpvwatcher did not answer within {0:?}")]
    Timeout(Duration),
    #[error("connection to remoteplayer-mpvwatcher failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected answer from remoteplayer-mpvwatcher: {0}")]
    Protocol(String),
    #[error(
        "remoteplayer-mpvwatcher speaks format version {found}, this service understands {SUPPORTED_STATE_FORMAT_VERSION}"
    )]
    UnsupportedVersion { found: u32 },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct LastReply {
    format_version: u32,
    known: bool,
    #[serde(default)]
    state: Option<SavedPlayerState>,
}

pub struct WatcherClient {
    socket: PathBuf,
}

impl WatcherClient {
    pub fn new(socket: PathBuf) -> WatcherClient {
        WatcherClient { socket }
    }

    /// `Ok(None)`: the watcher knows no state (e.g. it has no writable state file
    /// and was started after the last playback).
    pub async fn last(&self) -> Result<Option<SavedPlayerState>, WatcherError> {
        let answer: Result<Result<String, WatcherError>, tokio::time::error::Elapsed> =
            tokio::time::timeout(REQUEST_TIMEOUT, self.request("last")).await;
        let line: String =
            answer.map_err(|_elapsed: tokio::time::error::Elapsed| WatcherError::Timeout(REQUEST_TIMEOUT))??;
        let reply: LastReply = serde_json::from_str(&line)
            .map_err(|error: serde_json::Error| WatcherError::Protocol(error.to_string()))?;
        if reply.format_version != SUPPORTED_STATE_FORMAT_VERSION {
            return Err(WatcherError::UnsupportedVersion { found: reply.format_version });
        }
        match (reply.known, reply.state) {
            (true, Some(state)) => Ok(Some(state)),
            (true, None) => Err(WatcherError::Protocol("`known` is true but there is no `state`".to_string())),
            (false, _) => Ok(None),
        }
    }

    /// Like `last`, but keeps retrying for up to `patience` while the watcher is
    /// not reachable yet — at boot it may start after this service.
    pub async fn last_waiting(&self, patience: Duration) -> Result<Option<SavedPlayerState>, WatcherError> {
        let deadline: tokio::time::Instant = tokio::time::Instant::now() + patience;
        loop {
            match self.last().await {
                Err(WatcherError::Unreachable { .. }) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                other => return other,
            }
        }
    }

    async fn request(&self, request: &str) -> Result<String, WatcherError> {
        let stream: UnixStream = UnixStream::connect(&self.socket).await.map_err(|source: std::io::Error| {
            WatcherError::Unreachable { socket: self.socket.display().to_string(), source }
        })?;
        let (read_half, mut write_half): (OwnedReadHalf, OwnedWriteHalf) = stream.into_split();
        write_half.write_all(format!("{request}\n").as_bytes()).await?;
        let mut line: String = String::new();
        let read: usize = BufReader::new(read_half).read_line(&mut line).await?;
        if read == 0 {
            return Err(WatcherError::Protocol("the watcher closed the connection without answering".to_string()));
        }
        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    async fn answer_once(listener: UnixListener, reply: &'static str) {
        let (stream, _address): (UnixStream, tokio::net::unix::SocketAddr) = listener.accept().await.expect("accept");
        let (read_half, mut write_half): (OwnedReadHalf, OwnedWriteHalf) = stream.into_split();
        let mut request: String = String::new();
        BufReader::new(read_half).read_line(&mut request).await.expect("request");
        assert_eq!(request, "last\n");
        write_half.write_all(reply.as_bytes()).await.expect("reply");
    }

    #[tokio::test]
    async fn unknown_state_is_none() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let socket: PathBuf = directory.path().join("state.sock");
        tokio::spawn(answer_once(
            UnixListener::bind(&socket).expect("bind"),
            "{\"format-version\":1,\"known\":false}\n",
        ));
        let last: Option<SavedPlayerState> = WatcherClient::new(socket).last().await.expect("answer");
        assert!(last.is_none());
    }

    #[tokio::test]
    async fn known_state_is_parsed() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let socket: PathBuf = directory.path().join("state.sock");
        let reply: &'static str = r#"{"format-version":1,"known":true,"state":{"format-version":1,"captured-at-unix-ms":5,"sequence":2,"mpv":{"pid":7,"version":null},"player":{"playlist":[{"filename":"/a.mkv"}],"playlist-pos":0,"time-pos":3.5,"paused":false,"volume":90.0,"muted":false,"speed":1.0,"audio-track":"auto","subtitle-track":"no","secondary-subtitle-track":"no","audio-delay":0.0,"subtitle-delay":0.0}}}
"#;
        tokio::spawn(answer_once(UnixListener::bind(&socket).expect("bind"), reply));
        let last: SavedPlayerState = WatcherClient::new(socket).last().await.expect("answer").expect("known");
        assert_eq!(last.mpv.pid, Some(7));
        assert_eq!(last.player.time_pos, Some(3.5));
    }

    #[tokio::test]
    async fn absent_watcher_is_unreachable_after_waiting() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let client: WatcherClient = WatcherClient::new(directory.path().join("absent.sock"));
        let result: Result<Option<SavedPlayerState>, WatcherError> =
            client.last_waiting(Duration::from_millis(600)).await;
        assert!(matches!(result, Err(WatcherError::Unreachable { .. })));
    }
}
