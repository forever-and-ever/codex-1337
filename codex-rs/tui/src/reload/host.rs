//! The terminal owner. No worker, network operation, or reaper is awaited here.
//! All output is gated by generation; stale workers only own private PTYs.

use super::admission::Admission;
use super::admission::Origin;
use super::admission::Reload;
use super::broker;
use super::control;
use super::deadline::Deadline;
use super::deadline::Transition;
use super::retained::PROTOCOL;
use super::terminal::RESET_TERMINAL;
use super::terminal::RealTerminal;
use super::worker_process;
use super::worker_process::Launch;
use super::worker_process::Worker;
use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

const RESTARTING: &[u8] = b"\x1b[?1049h\x1b[2J\x1b[HRestarting...";
const FAILED: &[u8] = b"\x1b[?1049h\x1b[2J\x1b[HUI reload failed. Drafts are retained.\r\nR: retry update   B: previous UI   Ctrl+C: exit";
const CONFIRM_EXIT: &[u8] = b"\x1b[?1049h\x1b[2J\x1b[HExit and discard retained input? Y / N";

pub(super) async fn run() -> io::Result<i32> {
    // The proxy has the same authority as the shared app server. Keep both
    // sockets inside its existing sandbox-masked namespace, never ordinary /tmp.
    let directory = tempfile::Builder::new()
        .prefix("ui-")
        .tempdir_in(codex_uds::prepare_shared_daemon_socket_directory()?)?;
    let (control_tx, control_rx) = mpsc::channel(/*buffer*/ 16);
    let (broker_tx, broker_rx) = mpsc::channel(/*buffer*/ 256);
    let control_socket = tokio::net::UnixListener::bind(directory.path().join("control.sock"))?;
    let server_socket = tokio::net::UnixListener::bind(directory.path().join("server.sock"))?;
    let control_listener = tokio::spawn(control::listen(control_socket, control_tx));
    let commands = broker_tx.clone();
    let broker_listener = tokio::spawn(broker::listen(server_socket, commands));
    let broker_task = tokio::spawn(broker::run(broker_rx));
    let host = Host {
        directory: directory.path().to_owned(),
        previous: std::env::current_exe()?,
        installed: installed_entrypoint()?,
        worker: None,
        generation: 0,
        checkpoint: None,
        rejected_candidate: None,
        reload: Reload::Idle,
        size: crossterm::terminal::size()?,
        output: VecDeque::new(),
        pending_output: VecDeque::new(),
        input: VecDeque::new(),
        output_open: true,
        slots: std::sync::Arc::default(),
        reaper: worker_process::Reaper::new()?,
    };
    let _services = Services([
        control_listener.abort_handle(),
        broker_listener.abort_handle(),
        broker_task.abort_handle(),
    ]);
    let (finished, result) = oneshot::channel();
    std::thread::Builder::new()
        .name("ui-supervisor".into())
        .spawn(move || {
            let result = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => {
                    let result = runtime.block_on(supervise(host, control_rx, broker_tx));
                    // A blocked exec/reaper must not make runtime teardown another hidden wait.
                    runtime.shutdown_background();
                    result
                }
                Err(error) => Err(error),
            };
            let _ = finished.send(result);
        })?;
    result.await.map_err(io::Error::other)?
}

struct Services([tokio::task::AbortHandle; 3]);

impl Drop for Services {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

async fn supervise(
    mut host: Host,
    mut control_rx: mpsc::Receiver<control::Received>,
    broker_tx: mpsc::Sender<broker::Command>,
) -> io::Result<i32> {
    let (launched, mut launches) = mpsc::channel(/*buffer*/ 2);
    let terminal = RealTerminal::new()?;
    let mut resize = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut exits = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    host.launch(host.previous.clone(), &launched);
    let mut exiting: Option<(i32, Instant)> = None;
    let mut confirm_exit = false;
    let exit = loop {
        if let Some((code, deadline)) = exiting
            && ((!host.output_open && host.output.is_empty()) || Instant::now() >= deadline)
        {
            break code;
        }
        // This always precedes input/output work, even under a continuous event flood.
        let mut wake_after = match &mut host.reload {
            Reload::Checking(admission) => {
                let now = Instant::now();
                if now >= admission.expires {
                    host.reject_admission(&io::Error::new(
                        io::ErrorKind::TimedOut,
                        "UI checkpoint probe timed out",
                    ));
                    None
                } else {
                    Some(admission.expires.saturating_duration_since(now))
                }
            }
            Reload::Replacing { deadline, .. } => match deadline.poll(Instant::now()) {
                Transition::Wait(duration) => Some(duration),
                Transition::Rollback => {
                    host.rollback(&launched);
                    Some(Duration::ZERO)
                }
                Transition::Failed => {
                    host.fail();
                    None
                }
            },
            Reload::Idle | Reload::Failed => None,
        };
        if let Some((_, deadline)) = exiting {
            wake_after = Some(deadline.saturating_duration_since(Instant::now()));
        }
        tokio::select! {
            _ = terminate.recv() => break 128 + libc::SIGTERM,
            _ = hangup.recv() => break 128 + libc::SIGHUP,
            () = async {
                match wake_after {
                    Some(duration) => tokio::time::sleep(duration).await,
                    None => std::future::pending().await,
                }
            } => {},
            _ = exits.recv() => {
                if let Some(worker) = &mut host.worker
                    && let Some(code) = worker.exit_code()?
                {
                    if !host.reload.replacing() && !host.reload.failed() {
                        if code != 0 && host.checkpoint.is_some() { host.fail(); }
                        else { exiting = Some((code, Instant::now() + Duration::from_millis(/*millis*/ 200))); }
                    } else { host.reject(&launched); }
                }
            }
            result = async {
                match &mut host.reload {
                    Reload::Checking(admission) => (&mut admission.result).await,
                    _ => std::future::pending().await,
                }
            } => {
                match result.unwrap_or_else(|_| Err(io::Error::other("UI probe task stopped"))) {
                    Ok(candidate) if exiting.is_none() => {
                        let Reload::Checking(admission) = std::mem::replace(&mut host.reload, Reload::Idle) else { unreachable!() };
                        let (idle_candidate, reply) = match admission.origin {
                            Origin::Worker { idle_candidate, reply } => (idle_candidate, Some(reply)),
                            Origin::Recovery => (None, None),
                        };
                        host.checkpoint = Some(admission.checkpoint);
                        if let Some(worker) = &host.worker { host.previous = worker.executable.clone(); }
                        host.reload = Reload::Replacing { deadline: Deadline::new(Instant::now()), idle_candidate };
                        host.launch(candidate, &launched);
                        // Releasing the old call before retiring its worker could let it
                        // consume queued input that is also present in the checkpoint.
                        drop(reply);
                    }
                    Ok(_) => host.reject_admission(&io::Error::other("UI exited during reload admission")),
                    Err(error) => host.reject_admission(&error),
                }
            }
            Some((generation, result)) = launches.recv() => {
                if generation == host.generation {
                    match result {
                        Ok(mut worker) => {
                            if let Err(error) = worker.resize(host.size) {
                                if host.reload.replacing() { host.reject(&launched); continue; }
                                return Err(error);
                            }
                            if let Some(code) = worker.exit_code()? {
                                if !host.reload.replacing() && !host.reload.failed() {
                                    exiting = Some((code, Instant::now() + Duration::from_millis(/*millis*/ 200)));
                                    host.worker = Some(worker);
                                } else { host.reject(&launched); }
                            } else { host.worker = Some(worker); }
                        },
                        Err(_) if host.reload.replacing() => host.reject(&launched),
                        Err(error) => return Err(error),
                    }
                }
            }
            Some(control::Received { envelope, reply }) = control_rx.recv() => {
                if envelope.generation != host.generation {
                    let _ = reply.send(control::Response::Error("Retired UI generation".into()));
                    continue;
                }
                let response = match envelope.request {
                    control::Request::Hello { protocol, rejection_handoff } if protocol == PROTOCOL => {
                        if rejection_handoff {
                            control::Response::State(Some(control::StatePayload::Handoff(control::State {
                                checkpoint: host.checkpoint.clone().unwrap_or(serde_json::Value::Null),
                                rejected_candidate: host.rejected_candidate.clone(),
                            })))
                        } else {
                            control::Response::State(host.checkpoint.clone().map(control::StatePayload::Legacy))
                        }
                    }
                    control::Request::Hello { .. } => control::Response::Error("Incompatible UI reload protocol".into()),
                    control::Request::Connect(endpoint) => {
                        let commands = broker_tx.clone();
                        tokio::spawn(async move {
                            let (tx, rx) = oneshot::channel();
                            let _ = commands.send(broker::Command::Connect(endpoint.into(), tx)).await;
                            let response = match rx.await {
                                Ok(Ok(())) => control::Response::Ok,
                                _ => control::Response::Error("Unable to connect to app server".into()),
                            };
                            let _ = reply.send(response);
                        });
                        continue;
                    }
                    control::Request::Reload(checkpoint) if host.reload.idle() => {
                        let (checkpoint, candidate) = match checkpoint {
                            control::ReloadPayload::Idle(reload) => (reload.checkpoint, Some(reload.idle_candidate)),
                            control::ReloadPayload::Manual(checkpoint) => (checkpoint, None),
                        };
                        let executable = candidate.clone().unwrap_or_else(|| host.installed.clone());
                        host.reload = Reload::Checking(Admission::new(checkpoint, executable,
                            Origin::Worker { reply, idle_candidate: candidate }, &host.slots));
                        // The writer remains live until the checkpoint reader is admitted.
                        continue;
                    }
                    control::Request::Reload(_) => control::Response::Error("UI reload already in progress".into()),
                    control::Request::Ready => {
                        host.reload = Reload::Idle;
                        // Input ownership has transferred. Never recover a later crash
                        // from this now-stale snapshot and resend already-used queues.
                        host.checkpoint = None;
                        // Show a replacement only once it can accept input. A
                        // half-ready frame must not invite keystrokes we discard.
                        host.output.append(&mut host.pending_output);
                        control::Response::Ok
                    }
                    control::Request::Suspend if host.reload.idle() => {
                        host.output.clear();
                        host.input.clear();
                        terminal.suspend()?;
                        control::Response::Ok
                    }
                    control::Request::Suspend => control::Response::Error("UI reload in progress".into()),
                };
                let _ = reply.send(response);
            }
            ready = terminal.input().readable(), if host.reload.replacing() || host.reload.failed() || host.input.len() <= 65536 - 4096 => {
                let mut ready = ready?;
                let mut bytes = [0; 4096];
                if let Ok(result) = ready.try_io(|fd| worker_process::read(fd.get_ref(), &mut bytes)) {
                    if result.as_ref().is_err_and(|error| error.kind() == io::ErrorKind::Interrupted) { continue; }
                    let count = result?;
                    if count == 0 { break 0 }
                    if host.reload.failed() {
                        if confirm_exit {
                            if bytes[..count].iter().any(|key| matches!(key, b'y' | b'Y')) { break 0 }
                            confirm_exit = false;
                            host.paint(FAILED);
                            continue;
                        }
                        if bytes[..count].iter().any(|key| matches!(key, 3 | 4)) {
                            confirm_exit = true;
                            host.paint(CONFIRM_EXIT);
                            continue;
                        }
                        if matches!(host.reload, Reload::Failed) && let Some(key) = bytes[..count].iter().find(|key| matches!(key, b'r' | b'R' | b'b' | b'B')) {
                            let executable = if matches!(key, b'b' | b'B') { &host.previous } else { &host.installed };
                            let executable = executable.clone();
                            if matches!(key, b'r' | b'R') {
                                if let Some(checkpoint) = host.checkpoint.clone() {
                                    host.reload = Reload::Checking(Admission::new(checkpoint, executable, Origin::Recovery, &host.slots));
                                }
                            } else {
                                // The previous writer can read its own checkpoint.
                                host.reload = Reload::Replacing { deadline: Deadline::new(Instant::now()), idle_candidate: None };
                                host.launch(executable, &launched);
                            }
                        }
                    } else if !host.reload.replacing() && host.input.len() + count <= 65536 {
                        host.input.extend(&bytes[..count]);
                    }
                }
            }
            ready = terminal.output().writable(), if !host.output.is_empty() => {
                let mut ready = ready?;
                let bytes = host.output.as_slices().0;
                if let Ok(result) = ready.try_io(|fd| worker_process::write(fd.get_ref(), bytes)) {
                    if result.as_ref().is_err_and(|error| error.kind() == io::ErrorKind::Interrupted) { continue; }
                    host.output.drain(..result?);
                }
            }
            ready = async {
                match &host.worker {
                    Some(worker) => worker.terminal().readable().await,
                    None => std::future::pending().await,
                }
            }, if host.output_open && (
                    if host.reload.replacing() { host.pending_output.len() < 1024 * 1024 }
                    else { host.output.len() < 65536 }
                ) => {
                let mut ready = ready?;
                let mut bytes = [0; 4096];
                match ready.try_io(|fd| worker_process::read(fd.get_ref(), &mut bytes)) {
                    Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {},
                    Ok(Ok(0) | Err(_)) => host.output_open = false,
                    Ok(Ok(count)) => {
                        if host.reload.replacing() { host.pending_output.extend(&bytes[..count]); }
                        else { host.output.extend(&bytes[..count]); }
                    }
                    Err(_) => {},
                }
            }
            ready = async {
                match &host.worker {
                    Some(worker) => worker.terminal().writable().await,
                    None => std::future::pending().await,
                }
            }, if !host.input.is_empty() => {
                let mut ready = ready?;
                if let Ok(result) = ready.try_io(|fd| worker_process::write(fd.get_ref(), host.input.as_slices().0)) {
                    match result {
                        Ok(count) => { host.input.drain(..count); }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
                        Err(error) => return Err(error),
                    }
                }
            }
            _ = resize.recv() => {
                if let Ok(size) = crossterm::terminal::size() {
                    host.size = size;
                    if let Some(worker) = &host.worker { let _ = worker.resize(size); }
                }
            }
        }
    };
    Ok(exit)
}

struct Host {
    directory: PathBuf,
    previous: PathBuf,
    installed: PathBuf,
    worker: Option<Worker>,
    generation: u64,
    checkpoint: Option<serde_json::Value>,
    rejected_candidate: Option<PathBuf>,
    reload: Reload,
    size: (u16, u16),
    output: VecDeque<u8>,
    pending_output: VecDeque<u8>,
    input: VecDeque<u8>,
    output_open: bool,
    slots: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    reaper: worker_process::Reaper,
}

type Launched = mpsc::Sender<(u64, io::Result<Worker>)>;

impl Host {
    fn paint(&mut self, screen: &[u8]) {
        self.output = RESET_TERMINAL.iter().chain(screen).copied().collect();
    }

    fn launch(&mut self, executable: PathBuf, launched: &Launched) {
        self.worker = None;
        self.pending_output.clear();
        self.output_open = true;
        if self.reload.replacing() {
            self.paint(RESTARTING);
        }
        self.input.clear();
        self.generation += 1;
        let Some(slot) = worker_process::Slot::reserve(&self.slots) else {
            self.fail();
            return;
        };
        let generation = self.generation;
        // Paths may be reused by development builds; only the initial image is
        // already known to speak this protocol. Installed bundles are immutable.
        let negotiate = self.generation > 1;
        let mut launch = Launch {
            executable,
            generation,
            args: std::env::args_os().skip(/*n*/ 1).collect(),
            directory: self.directory.clone(),
            size: self.size,
            slot,
            reaper: self.reaper.clone(),
        };
        let launched = launched.clone();
        // Even the protocol probe's spawn can block in the kernel. Keep the
        // entire launch off the watchdog reactor, with its slot held throughout.
        tokio::task::spawn_blocking(move || {
            let result = tokio::runtime::Handle::current().block_on(async {
                if negotiate {
                    launch.negotiate().await?;
                }
                launch.spawn()
            });
            let _ = launched.blocking_send((generation, result));
        });
    }

    fn reject(&mut self, launched: &Launched) {
        let transition = match &mut self.reload {
            Reload::Replacing { deadline, .. } => Some(deadline.reject(Instant::now())),
            _ => None,
        };
        match transition {
            Some(Transition::Rollback) => {
                self.rollback(launched);
            }
            _ => self.fail(),
        }
    }

    fn rollback(&mut self, launched: &Launched) {
        if let Reload::Replacing { idle_candidate, .. } = &mut self.reload
            && let Some(candidate) = idle_candidate.take()
        {
            self.rejected_candidate = Some(candidate);
        }
        self.launch(self.previous.clone(), launched);
    }

    fn reject_admission(&mut self, error: &io::Error) {
        if let Reload::Checking(admission) = std::mem::replace(&mut self.reload, Reload::Idle) {
            match admission.origin {
                Origin::Worker {
                    reply,
                    idle_candidate,
                } => {
                    if let Some(candidate) = idle_candidate {
                        self.rejected_candidate = Some(candidate);
                    }
                    let _ = reply.send(control::Response::Error(error.to_string()));
                }
                Origin::Recovery => self.reload = Reload::Failed,
            }
        }
    }

    fn fail(&mut self) {
        self.worker = None;
        self.pending_output.clear();
        self.generation += 1;
        self.reload = Reload::Failed;
        self.input.clear();
        self.paint(FAILED);
    }
}

fn installed_entrypoint() -> io::Result<PathBuf> {
    match codex_install_context::Distribution::running() {
        Some(distribution) => Ok(distribution.entrypoint()),
        None => std::env::current_exe(),
    }
}
