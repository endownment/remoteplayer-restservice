use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::mpv::protocol::{IncomingMessage, MpvEvent, MpvReply, encode_command, parse_incoming_line};

const RECONNECT_DELAY: Duration = Duration::from_millis(500);
const REQUEST_QUEUE_CAPACITY: usize = 64;
const EVENT_BUFFER_CAPACITY: usize = 256;
/// Replies to requests the client sends on its own (observe_property) carry this id
/// and are dropped; ids handed out to callers start at 1.
const INTERNAL_REQUEST_ID: u64 = 0;

type ReplyResult = Result<MpvReply, MpvError>;

#[derive(Debug, thiserror::Error)]
pub enum MpvError {
    #[error("mpv is not running or its IPC socket is not reachable")]
    NotConnected,
    #[error("mpv did not reply within {0:?}")]
    Timeout(Duration),
    #[error("the connection to mpv was lost before it replied")]
    ConnectionLost,
    #[error("mpv rejected the command: {0}")]
    Rejected(String),
    #[error("unexpected reply from mpv: {0}")]
    UnexpectedReply(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    /// `generation` grows by one with every successful (re)connection, which is
    /// how callers notice that mpv was restarted.
    Connected {
        generation: u64,
    },
}

struct PendingRequest {
    request_id: u64,
    line: String,
    reply: oneshot::Sender<ReplyResult>,
}

/// A cheap-to-clone handle to one long-lived connection to mpv.
///
/// The socket itself is owned by a background task (the "actor"); handles talk
/// to it through channels. That is why no Mutex around the socket is needed:
/// exactly one task ever touches it.
#[derive(Clone)]
pub struct MpvClient {
    // Ownership: Arc = atomically reference-counted shared ownership, the
    // equivalent of std::shared_ptr. Cloning MpvClient only bumps the counter.
    shared: Arc<ClientShared>,
}

struct ClientShared {
    requests: mpsc::Sender<PendingRequest>,
    events: broadcast::Sender<MpvEvent>,
    connection: watch::Receiver<ConnectionState>,
    next_request_id: AtomicU64,
    request_timeout: Duration,
}

impl MpvClient {
    /// Must be called inside a Tokio runtime: it spawns the connection task.
    pub fn spawn(socket_path: PathBuf, observed_properties: Vec<String>, request_timeout: Duration) -> MpvClient {
        let (request_sender, request_receiver): (mpsc::Sender<PendingRequest>, mpsc::Receiver<PendingRequest>) =
            mpsc::channel(REQUEST_QUEUE_CAPACITY);
        let (event_sender, _unused_receiver): (broadcast::Sender<MpvEvent>, broadcast::Receiver<MpvEvent>) =
            broadcast::channel(EVENT_BUFFER_CAPACITY);
        let (state_sender, state_receiver): (watch::Sender<ConnectionState>, watch::Receiver<ConnectionState>) =
            watch::channel(ConnectionState::Disconnected);

        let connection_task: ConnectionTask = ConnectionTask {
            socket_path,
            observed_properties,
            requests: request_receiver,
            events: event_sender.clone(),
            state: state_sender,
        };
        tokio::spawn(connection_task.run());

        MpvClient {
            shared: Arc::new(ClientShared {
                requests: request_sender,
                events: event_sender,
                connection: state_receiver,
                next_request_id: AtomicU64::new(INTERNAL_REQUEST_ID + 1),
                request_timeout,
            }),
        }
    }

    pub fn connection_state(&self) -> ConnectionState {
        *self.shared.connection.borrow()
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.connection_state(), ConnectionState::Connected { .. })
    }

    pub fn current_generation(&self) -> Option<u64> {
        match self.connection_state() {
            ConnectionState::Connected { generation } => Some(generation),
            ConnectionState::Disconnected => None,
        }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<MpvEvent> {
        self.shared.events.subscribe()
    }

    pub fn watch_connection(&self) -> watch::Receiver<ConnectionState> {
        self.shared.connection.clone()
    }

    /// Waits until a connection newer than `previous_generation` exists
    /// (any connection when `previous_generation` is `None`).
    pub async fn wait_for_connection_after(
        &self,
        previous_generation: Option<u64>,
        limit: Duration,
    ) -> Result<u64, MpvError> {
        let mut receiver: watch::Receiver<ConnectionState> = self.watch_connection();
        let waited: Result<
            Result<watch::Ref<'_, ConnectionState>, watch::error::RecvError>,
            tokio::time::error::Elapsed,
        > = tokio::time::timeout(
            limit,
            receiver.wait_for(|state: &ConnectionState| is_newer_connection(state, previous_generation)),
        )
        .await;
        match waited {
            Ok(Ok(state)) => match *state {
                ConnectionState::Connected { generation } => Ok(generation),
                ConnectionState::Disconnected => Err(MpvError::NotConnected),
            },
            Ok(Err(_closed)) => Err(MpvError::ConnectionLost),
            Err(_elapsed) => Err(MpvError::Timeout(limit)),
        }
    }

    /// Sends a command and returns mpv's reply as is, including failures
    /// reported by mpv itself (`error` other than "success").
    pub async fn send(&self, command: Vec<Value>) -> Result<MpvReply, MpvError> {
        if !self.is_connected() {
            return Err(MpvError::NotConnected);
        }
        let request_id: u64 = self.shared.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (reply_sender, reply_receiver): (oneshot::Sender<ReplyResult>, oneshot::Receiver<ReplyResult>) =
            oneshot::channel();
        let request: PendingRequest =
            PendingRequest { request_id, line: encode_command(request_id, &command), reply: reply_sender };
        // Ownership: `request` is moved into the channel; after this line this
        // function can no longer touch it, the connection task owns it.
        self.shared.requests.send(request).await.map_err(|_closed| MpvError::NotConnected)?;

        let limit: Duration = self.shared.request_timeout;
        match tokio::time::timeout(limit, reply_receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_sender_dropped)) => Err(MpvError::ConnectionLost),
            Err(_elapsed) => Err(MpvError::Timeout(limit)),
        }
    }

    /// Like `send`, but an mpv-side failure becomes `MpvError::Rejected`.
    pub async fn command(&self, command: Vec<Value>) -> Result<Value, MpvError> {
        let reply: MpvReply = self.send(command).await?;
        if reply.is_success() { Ok(reply.data) } else { Err(MpvError::Rejected(reply.error)) }
    }

    pub async fn get_property<T: DeserializeOwned>(&self, name: &str) -> Result<T, MpvError> {
        let data: Value = self.command(vec![json!("get_property"), json!(name)]).await?;
        serde_json::from_value(data)
            .map_err(|error: serde_json::Error| MpvError::UnexpectedReply(format!("{name}: {error}")))
    }

    /// `Ok(None)` when mpv says the property is currently unavailable
    /// (e.g. `duration` while idle) or its value is `null`.
    pub async fn get_property_optional<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>, MpvError> {
        let reply: MpvReply = self.send(vec![json!("get_property"), json!(name)]).await?;
        if reply.error == "property unavailable" || (reply.is_success() && reply.data.is_null()) {
            return Ok(None);
        }
        if !reply.is_success() {
            return Err(MpvError::Rejected(format!("{name}: {}", reply.error)));
        }
        serde_json::from_value(reply.data)
            .map(Some)
            .map_err(|error: serde_json::Error| MpvError::UnexpectedReply(format!("{name}: {error}")))
    }

    pub async fn set_property(&self, name: &str, value: Value) -> Result<(), MpvError> {
        self.command(vec![json!("set_property"), json!(name), value]).await?;
        Ok(())
    }
}

fn is_newer_connection(state: &ConnectionState, previous_generation: Option<u64>) -> bool {
    match (state, previous_generation) {
        (ConnectionState::Connected { generation }, Some(previous)) => *generation > previous,
        (ConnectionState::Connected { .. }, None) => true,
        (ConnectionState::Disconnected, _) => false,
    }
}

/// The actor that owns the socket. It lives for the whole process lifetime and
/// reconnects forever, because mpv may be restarted at any moment.
struct ConnectionTask {
    socket_path: PathBuf,
    observed_properties: Vec<String>,
    requests: mpsc::Receiver<PendingRequest>,
    events: broadcast::Sender<MpvEvent>,
    state: watch::Sender<ConnectionState>,
}

enum ConnectionEnd {
    Lost,
    AllHandlesDropped,
}

enum Wakeup {
    Request(Option<PendingRequest>),
    Line(std::io::Result<Option<String>>),
}

impl ConnectionTask {
    // Ownership: `self` (not `&self`) — the task consumes the struct and owns its
    // fields until it ends; nobody else can reach them.
    async fn run(mut self) {
        let mut generation: u64 = 0;
        loop {
            if let Ok(stream) = UnixStream::connect(&self.socket_path).await {
                generation += 1;
                tracing::info!(socket = %self.socket_path.display(), generation, "connected to mpv");
                let end: ConnectionEnd = self.serve(stream, generation).await;
                self.state.send_replace(ConnectionState::Disconnected);
                if let ConnectionEnd::AllHandlesDropped = end {
                    return;
                }
                tracing::warn!("connection to mpv lost, reconnecting");
            }
            if !self.reject_queued_requests() {
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    }

    /// Returns `false` when every MpvClient handle is gone and the task should stop.
    fn reject_queued_requests(&mut self) -> bool {
        loop {
            match self.requests.try_recv() {
                Ok(request) => {
                    let _ignored: Result<(), ReplyResult> = request.reply.send(Err(MpvError::NotConnected));
                }
                Err(mpsc::error::TryRecvError::Empty) => return true,
                Err(mpsc::error::TryRecvError::Disconnected) => return false,
            }
        }
    }

    async fn serve(&mut self, stream: UnixStream, generation: u64) -> ConnectionEnd {
        let (read_half, mut write_half): (OwnedReadHalf, OwnedWriteHalf) = stream.into_split();
        let mut lines: Lines<BufReader<OwnedReadHalf>> = BufReader::new(read_half).lines();
        let mut pending: HashMap<u64, oneshot::Sender<ReplyResult>> = HashMap::new();

        if self.observe_properties(&mut write_half).await.is_err() {
            return ConnectionEnd::Lost;
        }
        self.state.send_replace(ConnectionState::Connected { generation });

        let end: ConnectionEnd = loop {
            // The select only decides what happened; the reaction is outside of it,
            // so the borrows taken by the two futures end before we mutate state.
            let wakeup: Wakeup = tokio::select! {
                request = self.requests.recv() => Wakeup::Request(request),
                line = lines.next_line() => Wakeup::Line(line),
            };
            match wakeup {
                Wakeup::Request(None) => break ConnectionEnd::AllHandlesDropped,
                Wakeup::Request(Some(request)) => {
                    if write_half.write_all(request.line.as_bytes()).await.is_err() {
                        let _ignored: Result<(), ReplyResult> = request.reply.send(Err(MpvError::ConnectionLost));
                        break ConnectionEnd::Lost;
                    }
                    pending.insert(request.request_id, request.reply);
                }
                Wakeup::Line(Ok(Some(line))) => self.dispatch_line(&line, &mut pending),
                Wakeup::Line(Ok(None)) | Wakeup::Line(Err(_)) => break ConnectionEnd::Lost,
            }
        };

        // Ownership: `drain` moves every sender out of the map so each waiting
        // caller gets an answer instead of hanging until its timeout.
        for (_request_id, reply) in pending.drain() {
            let _ignored: Result<(), ReplyResult> = reply.send(Err(MpvError::ConnectionLost));
        }
        end
    }

    async fn observe_properties(&self, write_half: &mut OwnedWriteHalf) -> std::io::Result<()> {
        for (index, property) in self.observed_properties.iter().enumerate() {
            let observer_id: u64 = index as u64 + 1;
            let line: String =
                encode_command(INTERNAL_REQUEST_ID, &[json!("observe_property"), json!(observer_id), json!(property)]);
            write_half.write_all(line.as_bytes()).await?;
        }
        Ok(())
    }

    fn dispatch_line(&self, line: &str, pending: &mut HashMap<u64, oneshot::Sender<ReplyResult>>) {
        match parse_incoming_line(line) {
            Ok(IncomingMessage::Reply { request_id, reply }) => {
                if let Some(waiting) = pending.remove(&request_id) {
                    let _ignored: Result<(), ReplyResult> = waiting.send(Ok(reply));
                }
            }
            Ok(IncomingMessage::Event(event)) => {
                // `send` fails only when nobody is subscribed, which is fine.
                let _ignored: Result<usize, broadcast::error::SendError<MpvEvent>> = self.events.send(event);
            }
            Ok(IncomingMessage::Unrecognized) => {}
            Err(error) => tracing::debug!(%error, line, "ignoring unparsable line from mpv"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    /// A tiny stand-in for mpv: answers `get_property volume` with 55 and
    /// pushes one property-change event after the first request.
    async fn run_fake_mpv(listener: UnixListener) {
        let (stream, _address): (UnixStream, tokio::net::unix::SocketAddr) = listener.accept().await.expect("accept");
        let (read_half, mut write_half): (OwnedReadHalf, OwnedWriteHalf) = stream.into_split();
        let mut lines: Lines<BufReader<OwnedReadHalf>> = BufReader::new(read_half).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).expect("json request");
            let request_id: u64 = request["request_id"].as_u64().expect("request id");
            if request_id == INTERNAL_REQUEST_ID {
                continue;
            }
            let reply: String = format!("{{\"request_id\":{request_id},\"error\":\"success\",\"data\":55}}\n");
            let event: &str = "{\"event\":\"property-change\",\"id\":1,\"name\":\"pause\",\"data\":true}\n";
            write_half.write_all(reply.as_bytes()).await.expect("write reply");
            write_half.write_all(event.as_bytes()).await.expect("write event");
        }
    }

    #[tokio::test]
    async fn request_reply_and_events_flow_through_one_connection() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let socket_path: PathBuf = directory.path().join("mpv.sock");
        let listener: UnixListener = UnixListener::bind(&socket_path).expect("bind");
        tokio::spawn(run_fake_mpv(listener));

        let client: MpvClient = MpvClient::spawn(socket_path, vec!["pause".to_string()], Duration::from_secs(2));
        let generation: u64 = client.wait_for_connection_after(None, Duration::from_secs(2)).await.expect("connect");
        assert_eq!(generation, 1);

        let mut events: broadcast::Receiver<MpvEvent> = client.subscribe_events();
        let volume: f64 = client.get_property("volume").await.expect("volume");
        assert_eq!(volume, 55.0);

        let event: MpvEvent =
            tokio::time::timeout(Duration::from_secs(2), events.recv()).await.expect("event in time").expect("event");
        assert_eq!(event.property_change(), Some(("pause", &json!(true))));
    }

    #[tokio::test]
    async fn commands_fail_fast_while_disconnected() {
        let client: MpvClient =
            MpvClient::spawn(PathBuf::from("/nonexistent/mpv.sock"), Vec::new(), Duration::from_secs(1));
        let result: Result<Value, MpvError> = client.command(vec![json!("stop")]).await;
        assert!(matches!(result, Err(MpvError::NotConnected)));
    }
}
