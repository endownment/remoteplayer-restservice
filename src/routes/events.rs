//! `GET /api/v1/player/events` — a WebSocket that pushes mpv's state changes,
//! so clients do not have to poll `/player/state`.
//!
//! Messages (JSON text frames):
//! - `{"type": "state", "state": {...}}` on connect and after every reconnect to mpv
//! - `{"type": "mpv-event", "event": {...}}` every mpv event, e.g. property changes
//! - `{"type": "connection", "connected": bool}` when mpv goes away or comes back
//! - `{"type": "lagged", "skipped": n}` when this client was too slow and missed events

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};

use crate::mpv::{ConnectionState, MpvEvent, PlayerState};
use crate::routes::SharedContext;

/// mpv reports `time-pos` on every frame; once per second is plenty for a UI.
const THROTTLED_PROPERTIES: [(&str, Duration); 1] = [("time-pos", Duration::from_secs(1))];

pub async fn events(State(context): State<SharedContext>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket: WebSocket| stream_events(context, socket))
}

enum Wakeup {
    Mpv(Result<MpvEvent, RecvError>),
    Connection(Result<(), watch::error::RecvError>),
    Client(Option<Result<Message, axum::Error>>),
}

async fn stream_events(context: SharedContext, mut socket: WebSocket) {
    let mut events: broadcast::Receiver<MpvEvent> = context.mpv.subscribe_events();
    let mut connection: watch::Receiver<ConnectionState> = context.mpv.watch_connection();
    // A cloned receiver reports the current value as unseen; the initial state
    // message below already covers it.
    connection.borrow_and_update();
    let mut last_sent: HashMap<&'static str, Instant> = HashMap::new();

    if send_state(&context, &mut socket).await.is_err() {
        return;
    }
    loop {
        let wakeup: Wakeup = tokio::select! {
            event = events.recv() => Wakeup::Mpv(event),
            changed = connection.changed() => Wakeup::Connection(changed),
            incoming = socket.recv() => Wakeup::Client(incoming),
        };
        let delivered: Result<(), axum::Error> = match wakeup {
            Wakeup::Mpv(Ok(event)) => {
                if is_throttled(&event, &mut last_sent) {
                    Ok(())
                } else {
                    send_json(&mut socket, json!({ "type": "mpv-event", "event": event.to_json() })).await
                }
            }
            Wakeup::Mpv(Err(RecvError::Lagged(skipped))) => {
                send_json(&mut socket, json!({ "type": "lagged", "skipped": skipped })).await
            }
            Wakeup::Mpv(Err(RecvError::Closed)) | Wakeup::Connection(Err(_)) => break,
            Wakeup::Connection(Ok(())) => {
                let connected: bool = matches!(*connection.borrow_and_update(), ConnectionState::Connected { .. });
                match send_json(&mut socket, json!({ "type": "connection", "connected": connected })).await {
                    Ok(()) if connected => send_state(&context, &mut socket).await,
                    other => other,
                }
            }
            Wakeup::Client(None) | Wakeup::Client(Some(Err(_))) | Wakeup::Client(Some(Ok(Message::Close(_)))) => break,
            Wakeup::Client(Some(Ok(_))) => Ok(()),
        };
        if delivered.is_err() {
            break;
        }
    }
}

fn is_throttled(event: &MpvEvent, last_sent: &mut HashMap<&'static str, Instant>) -> bool {
    let Some((property, _value)): Option<(&str, &Value)> = event.property_change() else {
        return false;
    };
    let Some((name, interval)): Option<&(&'static str, Duration)> =
        THROTTLED_PROPERTIES.iter().find(|(name, _): &&(&'static str, Duration)| *name == property)
    else {
        return false;
    };
    let now: Instant = Instant::now();
    match last_sent.get(name) {
        Some(previous) if now.duration_since(*previous) < *interval => true,
        _ => {
            last_sent.insert(*name, now);
            false
        }
    }
}

async fn send_state(context: &SharedContext, socket: &mut WebSocket) -> Result<(), axum::Error> {
    let state: PlayerState =
        PlayerState::query(&context.mpv).await.unwrap_or_else(|_error| PlayerState::disconnected());
    send_json(socket, json!({ "type": "state", "state": state })).await
}

async fn send_json(socket: &mut WebSocket, value: Value) -> Result<(), axum::Error> {
    socket.send(Message::Text(value.to_string().into())).await
}
