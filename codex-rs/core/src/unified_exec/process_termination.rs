//! One backend termination owner, shared by synchronous requests and confirmed stop.
//! A caller's cancellation never abandons a termination already sent to a backend.

use super::ProcessHandle;
use super::UnifiedExecError;
use super::UnifiedExecProcess;
use std::sync::Arc;
use tokio::sync::watch;

enum Stop {
    Live,
    Requested,
    Confirmed,
    Failed(String),
}

pub(super) struct Termination(watch::Sender<Stop>);

impl Default for Termination {
    fn default() -> Self {
        Self(watch::channel(Stop::Live).0)
    }
}

impl Termination {
    pub(super) fn requested(&self) -> bool {
        !matches!(*self.0.borrow(), Stop::Live)
    }

    fn claim(&self) -> bool {
        self.0.send_if_modified(|state| {
            if matches!(state, Stop::Requested | Stop::Confirmed) {
                return false;
            }
            *state = Stop::Requested;
            true
        })
    }
}

fn request(process: &UnifiedExecProcess) {
    request_backend(&process.termination, &process.process_handle);
}

pub(super) fn backend_has_exited(process: &UnifiedExecProcess) -> bool {
    match &process.process_handle {
        ProcessHandle::Local(backend) => backend.has_exited(),
        ProcessHandle::ExecServer(_) => process.state_rx.borrow().has_exited,
    }
}

pub(super) fn request_backend(termination: &Arc<Termination>, backend: &ProcessHandle) {
    if !termination.claim() {
        return;
    }
    match backend {
        ProcessHandle::Local(backend) => {
            backend.terminate();
            termination.0.send_replace(Stop::Confirmed);
        }
        ProcessHandle::ExecServer(backend) => {
            let backend = Arc::clone(backend);
            let owner = Arc::clone(termination);
            tokio::spawn(async move {
                let terminal = match backend.terminate().await {
                    Ok(()) => Stop::Confirmed,
                    Err(error) => Stop::Failed(error.to_string()),
                };
                owner.0.send_replace(terminal);
            });
        }
    }
}

pub(super) fn terminate(process: &UnifiedExecProcess) {
    request(process);
    process.finish_termination();
}

pub(super) async fn confirmed(process: &UnifiedExecProcess) -> Result<(), UnifiedExecError> {
    request(process);
    let mut state = process.termination.0.subscribe();
    loop {
        let result = match &*state.borrow_and_update() {
            Stop::Confirmed => Some(Ok(())),
            Stop::Failed(message) => Some(Err(UnifiedExecError::process_failed(message.clone()))),
            Stop::Live | Stop::Requested => None,
        };
        if let Some(result) = result {
            result?;
            if let ProcessHandle::Local(backend) = &process.process_handle {
                // Kill submission is not child settlement. The existing local
                // reaper publishes after wait; transport failure may set the
                // presentation state's has_exited before that actual exit.
                let mut exits = process.state_updates();
                exits
                    .wait_for(|_| backend.has_exited())
                    .await
                    .map_err(|_| {
                        UnifiedExecError::process_failed("local process reaper closed".into())
                    })?;
            } else {
                // Remote acknowledgment retains the backend protocol's meaning.
                process.signal_exit(process.exit_code());
            }
            process.finish_termination();
            return Ok(());
        }
        if state.changed().await.is_err() {
            return Err(UnifiedExecError::process_failed(
                "termination custody closed".into(),
            ));
        }
    }
}
