//! Operations that change mpv's process: applying settings, restarting with
//! the playback state preserved, and restoring after mpv crashed.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{MutexGuard, watch};

use crate::app_context::AppContext;
use crate::config::RestoreConfig;
use crate::mpv::{ConnectionState, MpvError, PlayerSnapshot, SavedPlayerState};
use crate::service_manager::ServiceError;
use crate::settings::{ApplyMode, PlayerSettings, SettingChange, SettingsError};
use crate::watcher_client::{WatcherClient, WatcherError};

const LOG_LINES_ON_FAILURE: usize = 20;
/// How long mpv may take to open its window after the IPC socket appeared.
const VIDEO_OUTPUT_GRACE: Duration = Duration::from_secs(5);
const VIDEO_OUTPUT_POLL: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("another settings change or restart is in progress")]
    Busy,
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error("mpv did not come back after the restart: {source}")]
    MpvDidNotStart { source: MpvError, log: Vec<String> },
    #[error("mpv started, but its video output did not open within {VIDEO_OUTPUT_GRACE:?}")]
    VideoOutputFailed { log: Vec<String> },
    #[error("the new settings did not work ({cause}); previous settings restored, rollback restart {rollback}")]
    RolledBack { cause: String, rollback: &'static str, log: Vec<String> },
    #[error("mpv refused `{key}`: {reason}; nothing was changed")]
    LiveChangeRejected { key: &'static str, reason: String },
    #[error("no watcher is configured (restore.watcher-socket)")]
    NoWatcher,
    #[error("the watcher knows no previous state")]
    NothingToRestore,
    #[error(transparent)]
    Watcher(#[from] WatcherError),
}

impl LifecycleError {
    pub fn log(&self) -> &[String] {
        match self {
            LifecycleError::MpvDidNotStart { log, .. }
            | LifecycleError::VideoOutputFailed { log }
            | LifecycleError::RolledBack { log, .. } => log,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct RestartReport {
    pub generation: u64,
    pub restored_items: usize,
    pub restore_warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppliedBy {
    Live,
    Restart,
    /// mpv is not reachable; the value is in the arguments file for its next start.
    NextStart,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ChangeReport {
    #[serde(flatten)]
    pub change: SettingChange,
    pub applied: AppliedBy,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ApplyReport {
    pub settings: PlayerSettings,
    pub changes: Vec<ChangeReport>,
    pub restart: Option<RestartReport>,
    pub warnings: Vec<String>,
}

fn lock_player(context: &AppContext) -> Result<MutexGuard<'_, ()>, LifecycleError> {
    context.player_operation.try_lock().map_err(|_locked: tokio::sync::TryLockError| LifecycleError::Busy)
}

pub async fn restart_preserving_state(context: &AppContext) -> Result<RestartReport, LifecycleError> {
    // Ownership: the guard unlocks the mutex when it is dropped at the end of this
    // function — the `_guard` name keeps it alive (a bare `_` would drop it at once).
    let _guard: MutexGuard<'_, ()> = lock_player(context)?;
    restart_locked(context, &context.settings.current()).await
}

async fn restart_locked(context: &AppContext, settings: &PlayerSettings) -> Result<RestartReport, LifecycleError> {
    context.planned_restart.store(true, Ordering::SeqCst);
    let result: Result<RestartReport, LifecycleError> = restart_and_restore(context, settings).await;
    context.planned_restart.store(false, Ordering::SeqCst);
    result
}

async fn restart_and_restore(context: &AppContext, settings: &PlayerSettings) -> Result<RestartReport, LifecycleError> {
    let snapshot: Option<PlayerSnapshot> = if context.mpv.is_connected() {
        match PlayerSnapshot::capture(&context.mpv).await {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                tracing::warn!(%error, "cannot capture the player state before restarting");
                None
            }
        }
    } else {
        None
    };
    let previous_generation: Option<u64> = context.mpv.current_generation();

    context.service_manager.restart(settings.to_mpv_args()).await?;

    let limit: Duration = Duration::from_secs(context.config.mpv.restart_timeout_seconds);
    let generation: u64 = match context.mpv.wait_for_connection_after(previous_generation, limit).await {
        Ok(generation) => generation,
        Err(source) => {
            let log: Vec<String> = context.service_manager.recent_log(LOG_LINES_ON_FAILURE).await.unwrap_or_default();
            return Err(LifecycleError::MpvDidNotStart { source, log });
        }
    };
    if settings.force_window && !video_output_opened(context).await {
        let log: Vec<String> = context.service_manager.recent_log(LOG_LINES_ON_FAILURE).await.unwrap_or_default();
        return Err(LifecycleError::VideoOutputFailed { log });
    }

    let (restored_items, restore_warnings): (usize, Vec<String>) = match snapshot {
        Some(snapshot) if !snapshot.is_empty() => (snapshot.playlist.len(), snapshot.restore(&context.mpv).await),
        _ => (0, Vec::new()),
    };
    Ok(RestartReport { generation, restored_items, restore_warnings })
}

/// mpv rejects unknown options and malformed values at startup and exits, which
/// the socket wait above catches. A video output that cannot be opened (wrong
/// `vo`, `gpu-context` or DRM plane) is different: mpv keeps running idle with
/// its socket open. With `force-window` mpv opens the output right away, so a
/// missing `current-vo` reveals the problem.
async fn video_output_opened(context: &AppContext) -> bool {
    let deadline: tokio::time::Instant = tokio::time::Instant::now() + VIDEO_OUTPUT_GRACE;
    loop {
        let current_vo: Option<String> = context.mpv.get_property_optional("current-vo").await.ok().flatten();
        if current_vo.is_some() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(VIDEO_OUTPUT_POLL).await;
    }
}

pub async fn apply_settings(context: &AppContext, next: PlayerSettings) -> Result<ApplyReport, LifecycleError> {
    let _guard: MutexGuard<'_, ()> = lock_player(context)?;
    next.validate().map_err(SettingsError::from)?;
    let current: PlayerSettings = context.settings.current();
    let changes: Vec<SettingChange> = current.changes_to(&next);
    if changes.is_empty() {
        return Ok(ApplyReport { settings: current, changes: Vec::new(), restart: None, warnings: Vec::new() });
    }

    let previous_env_file: Option<String> = context.settings.read_env_file()?;
    context.settings.write_env_file(&next)?;
    let needs_restart: bool = changes.iter().any(|change: &SettingChange| change.apply_mode == ApplyMode::Restart);

    if needs_restart {
        return apply_with_restart(context, current, next, changes, previous_env_file).await;
    }

    let mut warnings: Vec<String> = Vec::new();
    let mut reports: Vec<ChangeReport> = Vec::new();
    for change in changes {
        let applied: AppliedBy = if !context.mpv.is_connected() {
            AppliedBy::NextStart
        } else {
            match context.mpv.set_property(change.key, change.to.clone()).await {
                Ok(()) => AppliedBy::Live,
                // mpv refusing the value means it is invalid; it must not reach the
                // arguments file, where it would stop the next mpv from starting.
                Err(MpvError::Rejected(reason)) => {
                    undo_live_changes(context, &reports).await;
                    restore_env_file(context, previous_env_file, &current)?;
                    return Err(LifecycleError::LiveChangeRejected { key: change.key, reason });
                }
                Err(error) => {
                    warnings.push(format!("{}: {error}; applies on the next mpv start", change.key));
                    AppliedBy::NextStart
                }
            }
        };
        reports.push(ChangeReport { change, applied });
    }
    context.settings.persist(&next)?;
    Ok(ApplyReport { settings: next, changes: reports, restart: None, warnings })
}

async fn undo_live_changes(context: &AppContext, applied: &[ChangeReport]) {
    for report in applied.iter().rev() {
        if let Err(error) = context.mpv.set_property(report.change.key, report.change.from.clone()).await {
            tracing::warn!(key = report.change.key, %error, "cannot undo a live setting change");
        }
    }
}

fn restore_env_file(
    context: &AppContext,
    previous_env_file: Option<String>,
    current: &PlayerSettings,
) -> Result<(), LifecycleError> {
    match previous_env_file {
        Some(text) => context.settings.write_env_file_text(&text)?,
        None => context.settings.write_env_file(current)?,
    }
    Ok(())
}

async fn apply_with_restart(
    context: &AppContext,
    current: PlayerSettings,
    next: PlayerSettings,
    changes: Vec<SettingChange>,
    previous_env_file: Option<String>,
) -> Result<ApplyReport, LifecycleError> {
    match restart_locked(context, &next).await {
        Ok(restart) => {
            context.settings.persist(&next)?;
            let reports: Vec<ChangeReport> = changes
                .into_iter()
                .map(|change: SettingChange| ChangeReport { change, applied: AppliedBy::Restart })
                .collect();
            Ok(ApplyReport { settings: next, changes: reports, restart: Some(restart), warnings: Vec::new() })
        }
        Err(cause) => {
            tracing::error!(error = %cause, "new mpv settings failed, rolling back");
            let log: Vec<String> = cause.log().to_vec();
            restore_env_file(context, previous_env_file, &current)?;
            let rollback: &'static str = match restart_locked(context, &current).await {
                Ok(_) => "succeeded",
                Err(error) => {
                    tracing::error!(%error, "rollback restart failed as well");
                    "failed"
                }
            };
            Err(LifecycleError::RolledBack { cause: cause.to_string(), rollback, log })
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct RestoreReport {
    pub captured_at_unix_ms: u64,
    pub source_pid: Option<u32>,
    pub restored_items: usize,
    pub warnings: Vec<String>,
}

pub async fn restore_from_watcher(context: &AppContext) -> Result<RestoreReport, LifecycleError> {
    let watcher: &WatcherClient = context.watcher.as_ref().ok_or(LifecycleError::NoWatcher)?;
    let saved: SavedPlayerState = watcher.last().await?.ok_or(LifecycleError::NothingToRestore)?;
    let _guard: MutexGuard<'_, ()> = lock_player(context)?;
    let warnings: Vec<String> = saved.player.restore(&context.mpv).await;
    Ok(RestoreReport {
        captured_at_unix_ms: saved.captured_at_unix_ms,
        source_pid: saved.mpv.pid,
        restored_items: saved.player.playlist.len(),
        warnings,
    })
}

/// How long to wait for the watcher's socket at boot, when it may start later.
const WATCHER_PATIENCE: Duration = Duration::from_secs(10);

/// Restores the watcher's snapshot whenever a new mpv appears that this
/// service did not start itself: on this service's start (`restore.on-start`,
/// e.g. after a reboot) and after mpv crashed (`restore.after-crash`).
///
/// One rule covers both cases: the new mpv must still be empty, and the
/// snapshot must come from a *different* mpv process and contain a playlist.
/// So a restore never overwrites what a user or another IPC client started,
/// and a deliberate stop (same process, empty snapshot) is respected.
pub fn spawn_automatic_restore(context: Arc<AppContext>) {
    let restore: &RestoreConfig = &context.config.restore;
    if !restore.after_crash && !restore.on_start {
        return;
    }
    if context.watcher.is_none() {
        tracing::info!("automatic restore is off: restore.watcher-socket is not configured");
        return;
    }
    tokio::spawn(watch_for_new_mpv(context));
}

async fn watch_for_new_mpv(context: Arc<AppContext>) {
    let mut connection: watch::Receiver<ConnectionState> = context.mpv.watch_connection();
    let mut first_connection: bool = true;
    loop {
        let state: ConnectionState = *connection.borrow_and_update();
        if let ConnectionState::Connected { .. } = state {
            let wanted: bool =
                if first_connection { context.config.restore.on_start } else { context.config.restore.after_crash };
            first_connection = false;
            if wanted && !context.planned_restart.load(Ordering::SeqCst) {
                restore_into_new_mpv(&context).await;
            }
        }
        if connection.changed().await.is_err() {
            return;
        }
    }
}

async fn restore_into_new_mpv(context: &AppContext) {
    let Some(watcher): Option<&WatcherClient> = context.watcher.as_ref() else { return };
    let Some(pid): Option<u32> = context.mpv.get_property_optional("pid").await.ok().flatten() else { return };
    let playlist_count: u64 = context.mpv.get_property_optional("playlist-count").await.ok().flatten().unwrap_or(0);
    if playlist_count > 0 {
        tracing::debug!(pid, "mpv already has a playlist, nothing to restore");
        return;
    }
    let saved: SavedPlayerState = match watcher.last_waiting(WATCHER_PATIENCE).await {
        Ok(Some(saved)) => saved,
        Ok(None) => {
            tracing::info!(pid, "the watcher knows no previous state; nothing to restore");
            return;
        }
        Err(error) => {
            tracing::warn!(pid, %error, "cannot ask the watcher for the previous state");
            return;
        }
    };
    if saved.player.is_empty() || saved.mpv.pid == Some(pid) {
        tracing::debug!(pid, snapshot_pid = ?saved.mpv.pid, "the snapshot is empty or belongs to this mpv; not restoring");
        return;
    }
    let _guard: MutexGuard<'_, ()> = context.player_operation.lock().await;
    let warnings: Vec<String> = saved.player.restore(&context.mpv).await;
    tracing::info!(pid, previous_pid = ?saved.mpv.pid, items = saved.player.playlist.len(), ?warnings, "restored playback");
}
