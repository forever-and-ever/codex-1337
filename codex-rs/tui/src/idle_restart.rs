//! Quiet-time detection of new immutable Linux bundles. Hosted UIs hand off their
//! input to the renderer supervisor; unhosted clients use the older exec path.
//!
//! A PID-scoped, single-use handoff keeps original launch options without replaying
//! the original prompt, fork, or picker. The shared server is never shut down.

use crate::cli::Cli;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::time::Instant;

pub const IDLE_RESTART_PROTOCOL: &str = "codex-tui-idle-restart-v1";
const HANDOFF_ENV: &str = "CODEX_TUI_IDLE_RESTART";
const IDLE: Duration = Duration::from_mins(/*mins*/ 5);
static REJOINING_THREAD: OnceLock<ThreadId> = OnceLock::new();

#[derive(Debug, Serialize, Deserialize)]
struct Handoff {
    pid: u32,
    thread: ThreadId,
    cwd: AbsolutePathBuf,
}

impl Handoff {
    fn apply(self, cli: &mut Cli) {
        cli.prompt = None;
        cli.images.clear();
        cli.cwd = Some(self.cwd.to_path_buf());
        cli.agents_overview = false;
        cli.resume_picker = false;
        cli.resume_last = false;
        cli.resume_session_id = Some(self.thread.to_string());
        cli.fork_picker = false;
        cli.fork_last = false;
        cli.fork_session_id = None;
    }
}

/// Consume a same-process restart handoff before startup options are cloned for recovery.
pub fn apply_handoff(cli: &mut Cli) -> io::Result<()> {
    let Some(value) = std::env::var_os(HANDOFF_ENV) else {
        return Ok(());
    };
    let handoff: Handoff = serde_json::from_str(&value.to_string_lossy())?;
    // Helpers inherit the environment, but must not inherit the resume action.
    let owns_handoff = handoff.pid == std::process::id();
    #[cfg(target_os = "linux")]
    let owns_handoff = owns_handoff || crate::reload::worker::adopts_legacy_handoff(handoff.pid);
    if owns_handoff && REJOINING_THREAD.set(handoff.thread).is_ok() {
        handoff.apply(cli);
    }
    Ok(())
}

/// Startup recovery must reattach without replaying stale launch-time settings.
pub(crate) fn rejoins(thread: ThreadId) -> bool {
    #[cfg(target_os = "linux")]
    if crate::reload::worker::rejoins(thread) {
        return true;
    }
    REJOINING_THREAD.get() == Some(&thread)
}

pub(crate) struct IdleRestart {
    installed: Option<PathBuf>,
    running: PathBuf,
    deadline: Option<Instant>,
    rejected: Option<PathBuf>,
}

impl IdleRestart {
    pub(crate) fn new() -> Self {
        let running = std::env::current_exe().unwrap_or_default();
        let installed = cfg!(target_os = "linux")
            .then(|| installed_entrypoint(&running))
            .flatten();
        Self {
            installed,
            running,
            deadline: None,
            rejected: {
                #[cfg(target_os = "linux")]
                {
                    crate::reload::worker::rejected_candidate()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    None
                }
            },
        }
    }

    pub(crate) fn postpone(&mut self) {
        self.deadline = Some(Instant::now() + IDLE);
    }

    pub(crate) fn set_ready(&mut self, ready: bool) {
        if ready {
            self.deadline.get_or_insert_with(|| Instant::now() + IDLE);
        } else {
            self.deadline = None;
        }
    }

    pub(crate) async fn tick(&self) {
        if self.installed.is_some()
            && let Some(deadline) = self.deadline
        {
            tokio::time::sleep_until(deadline).await;
        } else {
            std::future::pending::<()>().await;
        }
    }

    pub(crate) async fn replacement(&mut self) -> Option<PathBuf> {
        self.postpone();
        let candidate = std::fs::canonicalize(self.installed.as_ref()?).ok()?;
        if candidate == self.running || self.rejected.as_ref() == Some(&candidate) {
            return None;
        }
        #[cfg(target_os = "linux")]
        if crate::reload::worker::is_hosted() {
            return Some(candidate);
        }
        // Older bundles do not understand the handoff: never replay their original
        // command line. Probe before touching terminal state, once per candidate.
        let expected = format!("{IDLE_RESTART_PROTOCOL}\n");
        let result = tokio::time::timeout(Duration::from_secs(/*secs*/ 2), async {
            let mut child = tokio::process::Command::new(&candidate)
                .args(["debug", "tui-restart-probe"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let mut stdout = Vec::new();
            child
                .stdout
                .take()
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?
                .take(expected.len() as u64)
                .read_to_end(&mut stdout)
                .await?;
            Ok::<_, io::Error>(std::process::Output {
                status: child.wait().await?,
                stdout,
                stderr: Vec::new(),
            })
        })
        .await;
        match result {
            Ok(Ok(output)) if output.status.success() && output.stdout == expected.as_bytes() => {
                Some(candidate)
            }
            Ok(Ok(_)) => {
                self.rejected = Some(candidate);
                None
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "could not probe the installed TUI; will retry after idle interval");
                None
            }
            Err(_) => None,
        }
    }

    pub(crate) fn reject(&mut self, candidate: PathBuf, error: &io::Error) {
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted | io::ErrorKind::OutOfMemory
        ) {
            self.postpone();
            return;
        }
        tracing::warn!(%error, "idle TUI replacement failed; keeping the current client");
        self.rejected = Some(candidate);
    }
}

fn installed_entrypoint(running: &Path) -> Option<PathBuf> {
    let distribution = codex_install_context::Distribution::of(running).ok()??;
    Some(distribution.entrypoint())
}

pub(crate) fn reexec(executable: &Path, thread: ThreadId, cwd: AbsolutePathBuf) -> io::Error {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let handoff = Handoff {
            pid: std::process::id(),
            thread,
            cwd,
        };
        let encoded = match serde_json::to_string(&handoff) {
            Ok(encoded) => encoded,
            Err(error) => return error.into(),
        };
        // The next process must capture cooked termios, not inherit raw mode as
        // its apparent original state. with_restored re-enables modes on failure.
        if let Err(error) = crossterm::terminal::disable_raw_mode() {
            return error;
        }
        std::process::Command::new(executable)
            .args(std::env::args_os().skip(1))
            .env(HANDOFF_ENV, encoded)
            .exec()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (executable, thread, cwd);
        io::Error::other("idle TUI replacement is Linux-only")
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "idle_restart_tests.rs"]
mod tests;
