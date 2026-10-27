//! Development stand-in for systemd: runs mpv as a child process, restarts it
//! with new arguments on request and respawns it when it exits, like
//! `Restart=always` does.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use crate::service_manager::{ServiceError, ServiceStatus};

const LOG_CAPACITY: usize = 500;
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(5);

type SharedLog = Arc<Mutex<VecDeque<String>>>;
type RestartResult = Result<(), ServiceError>;

enum SupervisorCommand {
    Restart { args: Vec<String>, done: oneshot::Sender<RestartResult> },
    Status { reply: oneshot::Sender<Option<u32>> },
}

/// A handle to the supervisor task, which alone owns the `Child`.
pub struct ChildProcessManager {
    commands: mpsc::Sender<SupervisorCommand>,
    log: SharedLog,
}

impl ChildProcessManager {
    pub fn spawn(
        command: PathBuf,
        fixed_args: Vec<String>,
        ipc_socket: PathBuf,
        respawn_delay: Duration,
    ) -> ChildProcessManager {
        let (sender, receiver): (mpsc::Sender<SupervisorCommand>, mpsc::Receiver<SupervisorCommand>) = mpsc::channel(8);
        let log: SharedLog = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAPACITY)));
        let supervisor: Supervisor = Supervisor {
            command,
            fixed_args,
            ipc_socket,
            respawn_delay,
            log: Arc::clone(&log),
            current_args: None,
            child: None,
            respawn_at: None,
        };
        tokio::spawn(supervisor.run(receiver));
        ChildProcessManager { commands: sender, log }
    }

    pub async fn restart(&self, args: Vec<String>) -> Result<(), ServiceError> {
        let (done, finished): (oneshot::Sender<RestartResult>, oneshot::Receiver<RestartResult>) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Restart { args, done })
            .await
            .map_err(|_closed| ServiceError::SupervisorGone)?;
        finished.await.map_err(|_dropped: oneshot::error::RecvError| ServiceError::SupervisorGone)?
    }

    pub async fn status(&self) -> Result<ServiceStatus, ServiceError> {
        let (reply, answer): (oneshot::Sender<Option<u32>>, oneshot::Receiver<Option<u32>>) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Status { reply })
            .await
            .map_err(|_closed| ServiceError::SupervisorGone)?;
        let pid: Option<u32> =
            answer.await.map_err(|_dropped: oneshot::error::RecvError| ServiceError::SupervisorGone)?;
        let (active_state, sub_state): (&str, &str) =
            if pid.is_some() { ("active", "running") } else { ("inactive", "dead") };
        Ok(ServiceStatus {
            manager: "child-process",
            unit: None,
            active_state: active_state.to_string(),
            sub_state: sub_state.to_string(),
            autostart: None,
            main_pid: pid,
        })
    }

    pub fn recent_log(&self, lines: usize) -> Vec<String> {
        let log: std::sync::MutexGuard<'_, VecDeque<String>> = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        let skip: usize = log.len().saturating_sub(lines);
        log.iter().skip(skip).cloned().collect()
    }
}

struct Supervisor {
    command: PathBuf,
    fixed_args: Vec<String>,
    ipc_socket: PathBuf,
    respawn_delay: Duration,
    log: SharedLog,
    current_args: Option<Vec<String>>,
    child: Option<Child>,
    /// When the exited mpv is due to be started again. A deadline instead of a
    /// sleep, so a restart request arriving meanwhile is handled at once.
    respawn_at: Option<tokio::time::Instant>,
}

enum Wakeup {
    Command(Option<SupervisorCommand>),
    ChildExited(std::io::Result<ExitStatus>),
    RespawnDue,
}

impl Supervisor {
    async fn run(mut self, mut commands: mpsc::Receiver<SupervisorCommand>) {
        loop {
            let wakeup: Wakeup = tokio::select! {
                command = commands.recv() => Wakeup::Command(command),
                status = wait_for_exit(&mut self.child) => Wakeup::ChildExited(status),
                () = wait_until(self.respawn_at) => Wakeup::RespawnDue,
            };
            match wakeup {
                Wakeup::Command(None) => {
                    self.stop_child().await;
                    return;
                }
                Wakeup::Command(Some(SupervisorCommand::Restart { args, done })) => {
                    let result: RestartResult = self.restart(args).await;
                    let _ignored: Result<(), RestartResult> = done.send(result);
                }
                Wakeup::Command(Some(SupervisorCommand::Status { reply })) => {
                    let pid: Option<u32> = self.child.as_ref().and_then(Child::id);
                    let _ignored: Result<(), Option<u32>> = reply.send(pid);
                }
                Wakeup::ChildExited(status) => {
                    self.child = None;
                    self.append_log(format!("mpv exited: {status:?}"));
                    self.respawn_at = Some(tokio::time::Instant::now() + self.respawn_delay);
                }
                Wakeup::RespawnDue => {
                    self.respawn_at = None;
                    if let Some(args) = self.current_args.clone() {
                        if let Err(error) = self.start(args) {
                            self.append_log(format!("respawn failed: {error}"));
                        }
                    }
                }
            }
        }
    }

    async fn restart(&mut self, args: Vec<String>) -> Result<(), ServiceError> {
        self.respawn_at = None;
        self.stop_child().await;
        self.start(args)
    }

    fn start(&mut self, args: Vec<String>) -> Result<(), ServiceError> {
        if let Some(directory) = self.ipc_socket.parent() {
            let _ignored: std::io::Result<()> = std::fs::create_dir_all(directory);
        }
        let mut command: Command = Command::new(&self.command);
        command
            .args(&self.fixed_args)
            .arg(format!("--input-ipc-server={}", self.ipc_socket.display()))
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child: Child = command.spawn().map_err(|source: std::io::Error| ServiceError::Spawn {
            command: self.command.display().to_string(),
            source,
        })?;
        // Ownership: `take()` moves the pipe out of the Child (leaving None), so the
        // reader task can own it independently of the Child's lifetime.
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(collect_output(stdout, Arc::clone(&self.log)));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(collect_output(stderr, Arc::clone(&self.log)));
        }
        self.append_log(format!("started mpv (pid {:?}) with {args:?}", child.id()));
        self.child = Some(child);
        self.current_args = Some(args);
        Ok(())
    }

    /// SIGTERM first, so mpv can shut down cleanly; SIGKILL after the grace period.
    async fn stop_child(&mut self) {
        let Some(mut child): Option<Child> = self.child.take() else { return };
        if let Some(pid) = child.id().and_then(|id: u32| i32::try_from(id).ok()) {
            let _ignored: nix::Result<()> = kill(Pid::from_raw(pid), Signal::SIGTERM);
        }
        let graceful: Result<std::io::Result<ExitStatus>, tokio::time::error::Elapsed> =
            tokio::time::timeout(STOP_GRACE_PERIOD, child.wait()).await;
        if graceful.is_err() {
            let _ignored: std::io::Result<()> = child.kill().await;
        }
    }

    fn append_log(&self, line: String) {
        tracing::info!(target: "mpv-child", "{line}");
        push_line(&self.log, line);
    }
}

async fn wait_for_exit(child: &mut Option<Child>) -> std::io::Result<ExitStatus> {
    match child {
        Some(running) => running.wait().await,
        // No child: never complete, so select! only reacts to commands.
        None => std::future::pending::<std::io::Result<ExitStatus>>().await,
    }
}

async fn wait_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(instant) => tokio::time::sleep_until(instant).await,
        None => std::future::pending::<()>().await,
    }
}

async fn collect_output<R: AsyncRead + Unpin>(reader: R, log: SharedLog) {
    let mut lines: tokio::io::Lines<BufReader<R>> = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(target: "mpv-child", "{line}");
        push_line(&log, line);
    }
}

fn push_line(log: &SharedLog, line: String) {
    let mut guard: std::sync::MutexGuard<'_, VecDeque<String>> = log.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.len() == LOG_CAPACITY {
        guard.pop_front();
    }
    guard.push_back(line);
}
