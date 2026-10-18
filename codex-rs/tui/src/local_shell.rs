//! Explicit TUI-host shell commands never become agent input.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codex_shell_command::shell_detect::default_user_shell;
use codex_utils_pty::SpawnedProcess;
use codex_utils_pty::spawn_pipe_process_no_stdin;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const OUTPUT_LIMIT: usize = 1024 * 1024;
const EXIT_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalShellCommand(String);

impl LocalShellCommand {
    pub(crate) fn text(&self) -> &str {
        &self.0
    }
}

/// One admission capability shared by App and whichever composer is currently visible.
#[derive(Clone, Debug)]
pub(crate) struct LocalShellAdmission(Arc<Semaphore>);

impl Default for LocalShellAdmission {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(1)))
    }
}

impl LocalShellAdmission {
    pub(crate) fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        self.0.clone().try_acquire_owned().ok()
    }

    pub(crate) fn is_claimed(&self) -> bool {
        self.0.available_permits() == 0
    }
}

/// Owns the sole admission until execution ends or the unstarted command is restored.
#[derive(Debug)]
pub struct LocalShellSubmission {
    pub(crate) command: LocalShellCommand,
    pub(crate) lease: OwnedSemaphorePermit,
}

impl PartialEq for LocalShellSubmission {
    fn eq(&self, other: &Self) -> bool {
        self.command == other.command
            && Arc::ptr_eq(self.lease.semaphore(), other.lease.semaphore())
    }
}

impl Eq for LocalShellSubmission {}

#[derive(Debug, PartialEq, Eq)]
pub enum LocalShellRequest {
    Show,
    Run(LocalShellSubmission),
}

impl LocalShellRequest {
    pub(crate) fn parse(text: &str, lease: Option<OwnedSemaphorePermit>) -> Option<Self> {
        let command = text.trim_start().strip_prefix("!!")?.trim();
        Some(if command.is_empty() {
            Self::Show
        } else {
            Self::Run(LocalShellSubmission {
                command: LocalShellCommand(command.to_owned()),
                lease: lease?,
            })
        })
    }
}

#[derive(Debug)]
pub(crate) enum LocalShellUpdate {
    Stalled,
    Finished(LocalShellResult),
}

#[derive(Debug)]
pub(crate) struct LocalShellResult {
    pub(crate) command: LocalShellCommand,
    pub(crate) cwd: PathBuf,
    pub(crate) output: Vec<u8>,
    pub(crate) discarded: usize,
    pub(crate) outcome: Result<i32, String>,
    pub(crate) cancelled: bool,
}

impl LocalShellResult {
    pub(crate) fn failure(command: LocalShellCommand, cwd: PathBuf, error: String) -> Self {
        Self {
            command,
            cwd,
            output: Vec::new(),
            discarded: 0,
            outcome: Err(error),
            cancelled: false,
        }
    }
}

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    discarded: usize,
}

impl Capture {
    fn append(&mut self, bytes: &[u8]) {
        let retained = bytes.len().min(OUTPUT_LIMIT - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..retained]);
        self.discarded = self.discarded.saturating_add(bytes.len() - retained);
    }
}

enum Phase {
    Running,
    Stopping(Instant),
    Stalled,
    Draining {
        outcome: Result<i32, String>,
        deadline: Instant,
    },
}

impl Phase {
    fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Stopping(deadline) | Self::Draining { deadline, .. } => Some(*deadline),
            Self::Running | Self::Stalled => None,
        }
    }
}

pub(crate) async fn execute(
    command: LocalShellCommand,
    cwd: PathBuf,
    cancellation: CancellationToken,
    mut update: impl FnMut(LocalShellUpdate),
) {
    let shell = default_user_shell();
    let args = vec!["-c".to_owned(), command.text().to_owned()];
    let env = std::env::vars_os()
        .map(|(key, value)| {
            let key = key
                .into_string()
                .map_err(|_| "Host environment contains a non-UTF-8 name")?;
            let value = value
                .into_string()
                .map_err(|_| "Host environment contains a non-UTF-8 value")?;
            Ok((key, value))
        })
        .collect::<Result<HashMap<String, String>, &str>>();
    let env = match env {
        Ok(env) => env,
        Err(error) => {
            update(LocalShellUpdate::Finished(LocalShellResult::failure(
                command,
                cwd,
                error.to_owned(),
            )));
            return;
        }
    };
    let spawned =
        spawn_pipe_process_no_stdin(&shell.shell_path, &args, &cwd, &env, &None, &[]).await;
    let SpawnedProcess {
        session,
        mut stdout_rx,
        mut stderr_rx,
        mut exit_rx,
    } = match spawned {
        Ok(process) => process,
        Err(error) => {
            update(LocalShellUpdate::Finished(LocalShellResult::failure(
                command,
                cwd,
                error.to_string(),
            )));
            return;
        }
    };
    // Dropping this future drops the process-group owner, not its reaper.
    let mut capture = Capture::default();
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut phase = Phase::Running;
    let outcome = loop {
        tokio::select! {
            () = cancellation.cancelled(), if matches!(phase, Phase::Running) => {
                session.request_terminate();
                phase = Phase::Stopping(Instant::now() + EXIT_GRACE);
            }
            chunk = stdout_rx.recv(), if stdout_open => {
                match chunk {
                    Some(bytes) => capture.append(&bytes),
                    None => stdout_open = false,
                }
            }
            chunk = stderr_rx.recv(), if stderr_open => {
                match chunk {
                    Some(bytes) => capture.append(&bytes),
                    None => stderr_open = false,
                }
            }
            status = &mut exit_rx, if !matches!(phase, Phase::Draining { .. }) => {
                let outcome = status.map_err(|error| format!("Exit waiter failed: {error}")).and_then(|code| {
                    if code == -1 { Err("Process waiter could not establish exit status".to_owned()) } else { Ok(code) }
                });
                phase = Phase::Draining { outcome, deadline: Instant::now() + EXIT_GRACE };
            }
            () = wait_deadline(phase.deadline()) => {
                match phase {
                    Phase::Stopping(_) => {
                        phase = Phase::Stalled;
                        update(LocalShellUpdate::Stalled);
                    }
                    Phase::Draining { .. } => {
                        session.terminate();
                        break Err("Shell exited, but output did not close; process-group termination requested".to_owned());
                    }
                    Phase::Running | Phase::Stalled => unreachable!("Only stopping and draining have deadlines"),
                }
            }
        }
        if !stdout_open
            && !stderr_open
            && let Phase::Draining { outcome, .. } = phase
        {
            break outcome;
        }
    };
    session.terminate();
    update(LocalShellUpdate::Finished(LocalShellResult {
        command,
        cwd,
        output: capture.bytes,
        discarded: capture.discarded,
        outcome,
        cancelled: cancellation.is_cancelled(),
    }));
}

async fn wait_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
