//! Worker-side handoff. The installed UI resumes the existing thread without replaying CLI input.

use super::control;
use super::retained::PROTOCOL;
use super::retained::RetainedInput;
use crate::cli::Cli;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

static WORKER: OnceLock<Worker> = OnceLock::new();
static TERMINAL: OnceLock<super::retained::TerminalHints> = OnceLock::new();

struct Worker {
    directory: PathBuf,
    generation: u64,
    primary: Option<ThreadId>,
    retained: Mutex<Option<RetainedInput>>,
    activating: AtomicBool,
    rejection_handoff: RejectionHandoff,
}

enum RejectionHandoff {
    Legacy,
    Supported(Option<PathBuf>),
}

pub(crate) fn rejected_candidate() -> Option<PathBuf> {
    match &WORKER.get()?.rejection_handoff {
        RejectionHandoff::Legacy => None,
        RejectionHandoff::Supported(candidate) => candidate.clone(),
    }
}

pub fn is_hosted() -> bool {
    WORKER.get().is_some()
}

pub(crate) fn suspend_host() -> io::Result<()> {
    let worker = WORKER.get().ok_or_else(|| io::Error::other("No UI host"))?;
    control::suspend(&worker.directory, worker.generation)
}

pub fn should_host() -> bool {
    use std::io::IsTerminal;
    !is_direct_worker() && io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn is_direct_worker() -> bool {
    // Helpers and terminals launched by the UI inherit its environment, not its seat.
    std::env::var("CODEX_UI_PARENT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .is_some_and(|parent| {
            parent
                == rustix::process::getppid()
                    .map_or(0, |pid| pid.as_raw_nonzero().get().unsigned_abs())
        })
}

pub async fn initialize(cli: &mut Cli) -> io::Result<()> {
    if !is_direct_worker() {
        return Ok(());
    }
    let Some(directory) = std::env::var_os("CODEX_UI_HOST") else {
        return Ok(());
    };
    let generation = std::env::var("CODEX_UI_GENERATION")
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| io::Error::other("Missing UI generation"))?;
    let directory = PathBuf::from(directory);
    let response = request_at(
        &directory,
        generation,
        control::Request::Hello {
            protocol: PROTOCOL.into(),
            rejection_handoff: true,
        },
    )
    .await?;
    let (state, rejection_handoff) = match response {
        control::Response::State(Some(control::StatePayload::Handoff(state))) => (
            (!state.checkpoint.is_null()).then_some(state.checkpoint),
            RejectionHandoff::Supported(state.rejected_candidate),
        ),
        control::Response::State(Some(control::StatePayload::Legacy(state))) => {
            (Some(state), RejectionHandoff::Legacy)
        }
        control::Response::State(None) => (None, RejectionHandoff::Legacy),
        control::Response::Error(message) => return Err(io::Error::other(message)),
        control::Response::Ok => {
            return Err(io::Error::other("Missing UI checkpoint"));
        }
    };
    let retained = state
        .map(serde_json::from_value::<RetainedInput>)
        .transpose()
        .map_err(io::Error::other)?;
    if let Some(state) = &retained {
        if let Some(terminal) = state.terminal {
            let _ = TERMINAL.set(terminal);
        }
        state.prepare_cli(cli);
    }
    let primary = retained.as_ref().and_then(|state| state.primary);
    let activating = AtomicBool::new(retained.is_some());
    WORKER
        .set(Worker {
            directory,
            generation,
            primary,
            retained: Mutex::new(retained),
            activating,
            rejection_handoff,
        })
        .map_err(|_| io::Error::other("UI worker already initialized"))
}

pub(crate) fn rejoins(thread: ThreadId) -> bool {
    WORKER
        .get()
        .is_some_and(|worker| worker.primary == Some(thread))
}

pub(crate) fn adopts_legacy_handoff(pid: u32) -> bool {
    // A one-time exec upgrade becomes the host, so its first worker inherits
    // that handoff. Later generations use their checkpoint, never stale CLI input.
    WORKER.get().is_some_and(|worker| worker.generation == 1)
        && pid
            == rustix::process::getppid().map_or(0, |pid| pid.as_raw_nonzero().get().unsigned_abs())
}

pub(crate) fn is_activating() -> bool {
    WORKER
        .get()
        .is_some_and(|worker| worker.activating.load(Ordering::Acquire))
}

pub(crate) fn take_input() -> Option<RetainedInput> {
    WORKER
        .get()?
        .retained
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

pub(crate) fn has_unattached_input() -> bool {
    WORKER.get().is_some_and(|worker| {
        worker.primary.is_none()
            && worker
                .retained
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
    })
}

pub(crate) fn has_session_draft() -> bool {
    WORKER.get().is_some_and(|worker| {
        worker.primary.is_none()
            && worker
                .retained
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_some_and(|state| state.unstarted.is_some())
    })
}

pub(crate) fn terminal_hints() -> Option<super::retained::TerminalHints> {
    TERMINAL.get().copied()
}

pub(crate) fn cached_terminal_probe() -> Option<crate::terminal_probe::StartupProbe> {
    if !is_activating() {
        return None;
    }
    terminal_hints().map(|hints| crate::terminal_probe::StartupProbe {
        cursor_position: Some(ratatui::layout::Position::new(/*x*/ 0, /*y*/ 0)),
        default_colors: hints.colors,
        bracketed_paste: hints.bracketed_paste,
        alternate_scroll_enabled: hints.alternate_scroll_enabled,
        keyboard_enhancement_supported: hints.enhanced_keys,
        terminal_app_over_ssh: hints.terminal_app_over_ssh,
    })
}

pub(crate) fn note_terminal_probe(probe: &crate::terminal_probe::StartupProbe) {
    if is_hosted() {
        let _ = TERMINAL.set(super::retained::TerminalHints {
            colors: probe.default_colors,
            bracketed_paste: probe.bracketed_paste,
            alternate_scroll_enabled: probe.alternate_scroll_enabled,
            enhanced_keys: probe.keyboard_enhancement_supported,
            terminal_app_over_ssh: probe.terminal_app_over_ssh,
        });
    }
}

async fn request(request: control::Request) -> io::Result<control::Response> {
    let worker = WORKER.get().ok_or_else(|| {
        io::Error::other("This TUI has no reload host; restart once to enable /reload")
    })?;
    request_at(&worker.directory, worker.generation, request).await
}

async fn request_at(
    directory: &Path,
    generation: u64,
    request: control::Request,
) -> io::Result<control::Response> {
    let supervisor_owns_wait = request.supervisor_owns_wait();
    let response = control::request(
        directory,
        control::Envelope {
            generation,
            request,
        },
    );
    if supervisor_owns_wait {
        // A local timeout cannot prove that the host has not committed this checkpoint.
        response.await
    } else {
        tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 6), response)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UI host did not respond"))?
    }
}

pub(crate) async fn proxy(
    endpoint: RemoteAppServerEndpoint,
) -> io::Result<RemoteAppServerEndpoint> {
    let Some(worker) = WORKER.get() else {
        return Ok(endpoint);
    };
    match request(control::Request::Connect(endpoint.into())).await? {
        control::Response::Ok => Ok(RemoteAppServerEndpoint::UnixSocket {
            socket_path: AbsolutePathBuf::from_absolute_path(worker.directory.join("server.sock"))?,
        }),
        control::Response::Error(message) => Err(io::Error::other(message)),
        control::Response::State(_) => Err(io::Error::other("Unexpected UI host response")),
    }
}

pub(crate) async fn reload(
    state: RetainedInput,
    idle_candidate: Option<PathBuf>,
) -> io::Result<()> {
    let state = serde_json::to_value(state).map_err(io::Error::other)?;
    let payload = if let Some(idle_candidate) = idle_candidate
        && matches!(
            WORKER.get().map(|worker| &worker.rejection_handoff),
            Some(RejectionHandoff::Supported(_))
        ) {
        control::ReloadPayload::Idle(control::IdleReload {
            checkpoint: state,
            idle_candidate,
        })
    } else {
        control::ReloadPayload::Manual(state)
    };
    match request(control::Request::Reload(payload)).await? {
        control::Response::Error(message) => Err(io::Error::other(message)),
        _ => Err(io::Error::other("UI replacement returned unexpectedly")),
    }
}

pub(crate) async fn ready() -> io::Result<()> {
    match request(control::Request::Ready).await? {
        control::Response::Ok => {
            if let Some(worker) = WORKER.get() {
                worker.activating.store(false, Ordering::Release);
            }
            Ok(())
        }
        control::Response::Error(message) => Err(io::Error::other(message)),
        control::Response::State(_) => Err(io::Error::other("Unexpected UI host response")),
    }
}
