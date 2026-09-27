//! Worker-side handoff. The installed UI resumes the existing thread without replaying CLI input.

use super::control;
use super::retained::PROTOCOL;
use super::retained::RetainedInput;
use crate::cli::Cli;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::io;
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
    !is_direct_worker() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn is_direct_worker() -> bool {
    // Helpers and terminals launched by the UI inherit its environment, not its seat.
    std::env::var("CODEX_UI_PARENT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        // SAFETY: getppid has no arguments or memory preconditions.
        .is_some_and(|parent| parent == unsafe { libc::getppid() } as u32)
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
    let response = control::request(
        &directory,
        control::Envelope {
            generation,
            request: control::Request::Hello {
                protocol: PROTOCOL.into(),
            },
        },
    )
    .await?;
    let retained = match response {
        control::Response::State(state) => state
            .map(serde_json::from_value::<RetainedInput>)
            .transpose()
            .map_err(io::Error::other)?,
        control::Response::Error(message) => return Err(io::Error::other(message)),
        control::Response::Ok | control::Response::Tools(_) | control::Response::Tasks { .. } => {
            return Err(io::Error::other("Missing UI checkpoint"));
        }
    };
    if let Some(state) = &retained {
        if let Some(terminal) = state.terminal {
            let _ = TERMINAL.set(terminal);
        }
        cli.prompt = None;
        cli.images.clear();
        cli.worktree = false;
        if let Some(cwd) = &state.cwd {
            cli.cwd = Some(cwd.to_path_buf());
        }
        cli.agents_overview = state.overview;
        cli.resume_picker = false;
        cli.resume_last = false;
        cli.resume_session_id = state.primary.map(|thread| thread.to_string());
        cli.fork_picker = false;
        cli.fork_last = false;
        cli.fork_session_id = None;
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
        // SAFETY: getppid has no arguments or memory preconditions.
        && pid == unsafe { libc::getppid() } as u32
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
        keyboard_enhancement_supported: hints.enhanced_keys,
    })
}

pub(crate) fn note_terminal_probe(probe: &crate::terminal_probe::StartupProbe) {
    if is_hosted() {
        let _ = TERMINAL.set(super::retained::TerminalHints {
            colors: probe.default_colors,
            enhanced_keys: probe.keyboard_enhancement_supported,
        });
    }
}

async fn request(request: control::Request) -> io::Result<control::Response> {
    let worker = WORKER.get().ok_or_else(|| {
        io::Error::other("This TUI has no reload host; restart once to enable /reload")
    })?;
    tokio::time::timeout(
        std::time::Duration::from_secs(/*secs*/ 6),
        control::request(
            &worker.directory,
            control::Envelope {
                generation: worker.generation,
                request,
            },
        ),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UI host did not respond"))?
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
        control::Response::State(_)
        | control::Response::Tools(_)
        | control::Response::Tasks { .. } => Err(io::Error::other("Unexpected UI host response")),
    }
}

pub(crate) async fn reload(state: RetainedInput) -> io::Result<()> {
    let state = serde_json::to_value(state).map_err(io::Error::other)?;
    match request(control::Request::Reload(state)).await? {
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
        control::Response::State(_)
        | control::Response::Tools(_)
        | control::Response::Tasks { .. } => Err(io::Error::other("Unexpected UI host response")),
    }
}

pub(crate) async fn start_tools(
    params: codex_app_server_protocol::ThreadStartParams,
    requirement: Option<codex_config::McpServerRequirement>,
    events: crate::app_event_sender::AppEventSender,
) -> io::Result<serde_json::Value> {
    let response = request(control::Request::Tools {
        params: Box::new(params),
    })
    .await?;
    let config = match response {
        control::Response::Tools(config) => config,
        control::Response::Error(message) => return Err(io::Error::other(message)),
        _ => return Err(io::Error::other("Unexpected task-tools response")),
    };
    // The host chooses its listener; authorize that exact returned identity before
    // exposing its credentials/configuration to any thread or model.
    if let Some(requirement) = requirement {
        let raw: codex_config::RawMcpServerConfig =
            serde_json::from_value(config.clone()).map_err(io::Error::other)?;
        let server = codex_config::McpServerConfig::try_from(raw).map_err(io::Error::other)?;
        if !server.matches_requirement(&requirement) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Managed policy does not permit the UI host task-tools server",
            ));
        }
    }
    tokio::spawn(async move {
        let mut after = 0;
        let Some(worker) = WORKER.get() else { return };
        loop {
            match control::request(
                &worker.directory,
                control::Envelope {
                    generation: worker.generation,
                    request: control::Request::Tasks { after },
                },
            )
            .await
            {
                Ok(control::Response::Tasks { cursor, tasks }) => {
                    after = cursor;
                    for (thread, task_tools_available) in tasks {
                        let (registered, _) = tokio::sync::oneshot::channel();
                        events.send(crate::app_event::AppEvent::DynamicToolThreadStarted {
                            thread,
                            task_tools_available,
                            registered,
                        });
                    }
                }
                _ => tokio::time::sleep(std::time::Duration::from_secs(/*secs*/ 1)).await,
            }
        }
    });
    Ok(config)
}
