use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ApiError;
use crate::lifecycle::{self, RestartReport, RestoreReport};
use crate::mpv::{MpvReply, PlayerState, check_raw_command};
use crate::player_command::PlayerCommand;
use crate::routes::SharedContext;
use crate::routes::extract::{JsonBody, QueryParams};
use crate::service_manager::ServiceStatus;

const DEFAULT_LOG_LINES: usize = 50;
const MAX_LOG_LINES: usize = 1000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawIpcRequest {
    command: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    lines: Option<usize>,
}

#[derive(Serialize)]
pub struct CommandResponse {
    data: Value,
}

pub async fn state(State(context): State<SharedContext>) -> Result<Json<PlayerState>, ApiError> {
    Ok(Json(PlayerState::query(&context.mpv).await?))
}

pub async fn command(
    State(context): State<SharedContext>,
    JsonBody(command): JsonBody<PlayerCommand>,
) -> Result<Json<CommandResponse>, ApiError> {
    let mpv_command: Vec<Value> = command.to_mpv_command(&context.library).await?;
    let reply: MpvReply = context.mpv.send(mpv_command).await?;
    if !reply.is_success() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("mpv refused the command: {}", reply.error),
        ));
    }
    Ok(Json(CommandResponse { data: reply.data }))
}

/// Forwards a command to mpv and returns mpv's reply unchanged, including mpv
/// errors (HTTP 200 with `"error": "..."`), as a raw passthrough should.
pub async fn raw_ipc(
    State(context): State<SharedContext>,
    JsonBody(request): JsonBody<RawIpcRequest>,
) -> Result<Json<MpvReply>, ApiError> {
    if !context.config.mpv.raw_ipc_enabled {
        return Err(ApiError::forbidden("raw IPC is disabled (mpv.raw-ipc-enabled = false)"));
    }
    check_raw_command(&request.command)?;
    Ok(Json(context.mpv.send(request.command).await?))
}

pub async fn restart(State(context): State<SharedContext>) -> Result<Json<RestartReport>, ApiError> {
    Ok(Json(lifecycle::restart_preserving_state(&context).await?))
}

pub async fn restore(State(context): State<SharedContext>) -> Result<Json<RestoreReport>, ApiError> {
    Ok(Json(lifecycle::restore_from_watcher(&context).await?))
}

pub async fn service(State(context): State<SharedContext>) -> Result<Json<ServiceStatus>, ApiError> {
    Ok(Json(context.service_manager.status().await?))
}

pub async fn log(
    State(context): State<SharedContext>,
    QueryParams(query): QueryParams<LogQuery>,
) -> Result<Json<Vec<String>>, ApiError> {
    let lines: usize = query.lines.unwrap_or(DEFAULT_LOG_LINES).min(MAX_LOG_LINES);
    Ok(Json(context.service_manager.recent_log(lines).await?))
}
