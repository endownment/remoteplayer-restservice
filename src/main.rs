mod app_context;
mod audio;
mod auth;
mod cec;
mod config;
mod display;
mod error;
mod fs_util;
mod lifecycle;
mod mpv;
mod player_command;
mod power;
mod routes;
mod service_manager;
mod settings;
mod storage;
mod system_info;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use tokio::net::TcpListener;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::FromEnvError;

use crate::app_context::AppContext;
use crate::config::AppConfig;

#[derive(Debug, Parser)]
#[command(version, about = "REST API for the remoteplayer mpv instance")]
struct CommandLine {
    /// Path to the TOML configuration file.
    #[arg(long, default_value = "/etc/remoteplayer/restservice.toml")]
    config: PathBuf,
}

// `#[tokio::main]` wraps this async fn in a regular `fn main` that starts the
// Tokio runtime (a thread pool + event loop) and blocks on the future.
#[tokio::main]
async fn main() -> ExitCode {
    init_logging();
    let command_line: CommandLine = CommandLine::parse();
    match run(command_line).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(command_line: CommandLine) -> Result<(), Box<dyn std::error::Error>> {
    let config: AppConfig = AppConfig::load(&command_line.config)?;
    if config.auth.disabled {
        tracing::warn!("authentication is disabled; anyone who can reach the port controls the player");
    }
    let context: Arc<AppContext> = AppContext::initialize(config).await?;
    lifecycle::spawn_crash_recovery(Arc::clone(&context));

    let listener: TcpListener = TcpListener::bind(context.config.listen).await?;
    tracing::info!(address = %context.config.listen, "remoteplayer-restservice is listening");
    notify_systemd_ready();

    let router: axum::Router = routes::build_router(context);
    axum::serve(listener, router).with_graceful_shutdown(wait_for_shutdown_signal()).await?;
    tracing::info!("stopped");
    Ok(())
}

fn init_logging() {
    let filter: EnvFilter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_unset: FromEnvError| EnvFilter::new("info"));
    // journald stamps every line itself and does not render ANSI colors.
    let under_journald: bool = std::env::var_os("JOURNAL_STREAM").is_some();
    if under_journald {
        tracing_subscriber::fmt().with_env_filter(filter).without_time().with_ansi(false).init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

/// With `Type=notify` systemd waits for this message before it considers the
/// service started. Outside systemd `NOTIFY_SOCKET` is unset and this does nothing.
fn notify_systemd_ready() {
    if let Err(error) = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]) {
        tracing::warn!(%error, "cannot notify systemd");
    }
}

async fn wait_for_shutdown_signal() {
    let mut terminate: Signal = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::error!(%error, "cannot listen for SIGTERM; only Ctrl+C stops the service");
            let _ignored: std::io::Result<()> = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    tracing::info!("shutdown requested");
}
