//! Operations that change mpv's process: applying settings, restarting with
//! the playback state preserved, and restoring after mpv crashed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{MutexGuard, watch};

use crate::app_context::AppContext;
use crate::mpv::{ConnectionState, MpvError, PlayerSnapshot, SavedPlayerState, SavedStateError, read_saved_state};
use crate::service_manager::ServiceError;
use crate::settings::{ApplyMode, PlayerSettings, SettingChange, SettingsError};

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
    #[error("no watcher state file is configured (restore.watcher-state-file)")]
    NoWatcherStateFile,
    #[error(transparent)]
    SavedState(#[from] SavedStateError),
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
    let path: &Path = context.config.restore.watcher_state_file.as_deref().ok_or(LifecycleError::NoWatcherStateFile)?;
    let saved: SavedPlayerState = read_saved_state(path)?;
    let _guard: MutexGuard<'_, ()> = lock_player(context)?;
    let warnings: Vec<String> = saved.player.restore(&context.mpv).await;
    Ok(RestoreReport {
        captured_at_unix_ms: saved.captured_at_unix_ms,
        source_pid: saved.mpv.pid,
        restored_items: saved.player.playlist.len(),
        warnings,
    })
}

/// Restores the watcher's last snapshot when mpv comes back with a new pid
/// that this service did not cause. The snapshot must belong to the process
/// that died, and the new mpv must still be empty, so a restore never
/// overwrites something a user (or another IPC client) already started.
pub fn spawn_crash_recovery(context: Arc<AppContext>) {
    if !context.config.restore.after_crash {
        return;
    }
    let Some(state_file): Option<PathBuf> = context.config.restore.watcher_state_file.clone() else {
        tracing::info!("crash recovery is off: restore.watcher-state-file is not configured");
        return;
    };
    tokio::spawn(watch_for_crashes(context, state_file));
}

async fn watch_for_crashes(context: Arc<AppContext>, state_file: PathBuf) {
    let mut connection: watch::Receiver<ConnectionState> = context.mpv.watch_connection();
    let mut last_pid: Option<u32> = None;
    loop {
        let state: ConnectionState = *connection.borrow_and_update();
        if let ConnectionState::Connected { .. } = state {
            let pid: Option<u32> = context.mpv.get_property_optional("pid").await.ok().flatten();
            let previous: Option<u32> = last_pid;
            last_pid = pid.or(previous);
            let replaced: bool = matches!((previous, pid), (Some(old), Some(new)) if old != new);
            if replaced && !context.planned_restart.load(Ordering::SeqCst) {
                if let Some(old_pid) = previous {
                    recover_after_crash(&context, &state_file, old_pid).await;
                }
            }
        }
        if connection.changed().await.is_err() {
            return;
        }
    }
}

async fn recover_after_crash(context: &AppContext, state_file: &Path, crashed_pid: u32) {
    let saved: SavedPlayerState = match read_saved_state(state_file) {
        Ok(saved) => saved,
        Err(error) => {
            tracing::warn!(%error, "mpv was restarted externally, but there is no usable watcher snapshot");
            return;
        }
    };
    if saved.mpv.pid != Some(crashed_pid) || saved.player.is_empty() {
        tracing::info!(crashed_pid, snapshot_pid = ?saved.mpv.pid, "watcher snapshot does not describe the crashed mpv, not restoring");
        return;
    }
    let playlist_count: u64 = context.mpv.get_property_optional("playlist-count").await.ok().flatten().unwrap_or(0);
    if playlist_count > 0 {
        tracing::info!("new mpv already has a playlist, not restoring");
        return;
    }
    let _guard: MutexGuard<'_, ()> = context.player_operation.lock().await;
    let warnings: Vec<String> = saved.player.restore(&context.mpv).await;
    tracing::info!(crashed_pid, items = saved.player.playlist.len(), ?warnings, "restored playback after mpv crash");
}
