//! Custody, independent of the pollable `ProcessStore` and receipt retention.
//!
//! Each admitted launch has one cancellation-independent supervisor. The registry
//! retains the actual backend handle and hold until backend exit, output settlement
//! and the existing event/hook finalizer have all completed. It never retains a
//! Session. Tracked launch/finalizer futures can retain one and are drained on stop.
//! Fully detached daemons are outside backend custody; connected MCP servers and
//! post-turn MCP jobs/hooks are not process-lifetime holds.

use super::UnifiedExecError;
use super::process::UnifiedExecProcess;
use crate::runtime_holds::RuntimeHold;
use crate::runtime_holds::RuntimeHolds;
use crate::tools::context::ExecCommandToolOutput;
use futures::FutureExt;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Reader destruction is not EOF. Failed/interrupted readers require explicit
/// backend reconciliation before custody can be released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputTerminal {
    Open,
    Eof,
    Interrupted,
    Failed,
}

pub(crate) struct OutputTermination(watch::Sender<OutputTerminal>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum DrainState {
    Unstarted,
    Streaming,
    Complete,
    Failed,
}

pub(super) struct OutputDrain(watch::Sender<DrainState>);

impl Default for OutputDrain {
    fn default() -> Self {
        Self(watch::channel(DrainState::Unstarted).0)
    }
}

impl OutputDrain {
    fn fail(&self) {
        self.0.send_replace(DrainState::Failed);
    }
    pub(super) fn start(&self) {
        self.0.send_replace(DrainState::Streaming);
    }
    pub(super) fn notify_one(&self) {
        self.0.send_replace(DrainState::Complete);
    }
    pub(super) async fn notified(&self) {
        let _ = self
            .0
            .subscribe()
            .wait_for(|state| *state != DrainState::Streaming)
            .await;
    }
}

impl Default for OutputTermination {
    fn default() -> Self {
        Self(watch::channel(OutputTerminal::Open).0)
    }
}

impl OutputTermination {
    pub(crate) fn load(&self, _ordering: std::sync::atomic::Ordering) -> bool {
        *self.0.borrow() != OutputTerminal::Open
    }
    pub(crate) fn store(&self, closed: bool, _ordering: std::sync::atomic::Ordering) {
        self.0.send_replace(if closed {
            OutputTerminal::Eof
        } else {
            OutputTerminal::Open
        });
    }
    pub(super) fn fail(&self) {
        self.0.send_replace(OutputTerminal::Failed);
    }
    pub(super) fn interrupt(&self) {
        self.0.send_if_modified(|state| {
            if *state != OutputTerminal::Open {
                return false;
            }
            *state = OutputTerminal::Interrupted;
            true
        });
    }
}

impl UnifiedExecProcess {
    async fn wait_backend_and_output(&self) {
        let mut state = self.state_updates();
        let mut output = self.output_handles().output_closed.0.subscribe();
        loop {
            let terminal = *output.borrow_and_update();
            let reconciled = match terminal {
                OutputTerminal::Open => false,
                OutputTerminal::Eof => true,
                OutputTerminal::Interrupted | OutputTerminal::Failed => {
                    match self.terminate_confirmed().await {
                        Ok(()) => true,
                        Err(error) => {
                            tracing::error!(%error, "cannot reconcile interrupted output; retaining custody");
                            false
                        }
                    }
                }
            };
            state.borrow_and_update();
            if self.backend_has_exited() && reconciled {
                return;
            }
            tokio::select! {
                _ = state.changed() => {},
                _ = output.changed() => {},
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Finalization {
    Initial,
    Background,
    Complete,
    Failed,
}

enum ObserverKind {
    Stream,
    End,
}

struct OwnedProcess {
    process: Arc<UnifiedExecProcess>,
    hold: Arc<RuntimeHold>,
    finalization: watch::Sender<Finalization>,
}

#[derive(Default)]
pub(super) struct ProcessLifetimes {
    launches: Mutex<HashMap<i32, Arc<RuntimeHold>>>,
    // The cancellation token is also the registration seal. Register-and-spawn
    // and close-and-snapshot retain this same short mutex; no await under it.
    processes: Mutex<HashMap<usize, OwnedProcess>>,
    closing: CancellationToken,
    tasks: TaskTracker,
}

fn key(process: &Arc<UnifiedExecProcess>) -> usize {
    Arc::as_ptr(process) as usize
}

impl ProcessLifetimes {
    pub(super) async fn execute<F, Make>(
        self: &Arc<Self>,
        runtime: &RuntimeHolds,
        process_id: i32,
        make: Make,
    ) -> Result<ExecCommandToolOutput, UnifiedExecError>
    where
        F: Future<Output = Result<ExecCommandToolOutput, UnifiedExecError>> + Send + 'static,
        Make: FnOnce() -> F,
    {
        let hold =
            Arc::new(runtime.acquire().ok_or_else(|| {
                UnifiedExecError::process_failed("conversation is closing".into())
            })?);
        let result = {
            let _registration = self
                .processes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.closing.is_cancelled() {
                return Err(UnifiedExecError::process_failed(
                    "process custody is closing".into(),
                ));
            }
            self.launches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(process_id, Arc::clone(&hold));
            let future = make();
            let (tx, rx) = oneshot::channel();
            let owner = Arc::clone(self);
            self.tasks.spawn(async move {
                // An inner panic destroys the observer, not the registry's custody.
                let result = std::panic::AssertUnwindSafe(future).catch_unwind().await;
                owner
                    .launches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&process_id);
                let failed = result.is_err();
                let result = result.unwrap_or_else(|_| {
                    Err(UnifiedExecError::process_failed(
                        "execution task panicked".into(),
                    ))
                });
                let owned = {
                    let entries = owner
                        .processes
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    entries
                        .values()
                        .filter(|entry| Arc::ptr_eq(&entry.hold, &hold))
                        .map(|entry| {
                            if failed {
                                entry.finalization.send_replace(Finalization::Failed);
                            } else if *entry.finalization.borrow() == Finalization::Initial {
                                entry.finalization.send_replace(Finalization::Complete);
                            }
                            (Arc::clone(&entry.process), entry.finalization.subscribe())
                        })
                        .collect::<Vec<_>>()
                };
                let _ = tx.send(result);
                for (process, mut finalized) in owned {
                    if failed {
                        let _ = process.terminate_confirmed().await;
                    }
                    let settled = async {
                        let failed_finalizer = finalized
                            .wait_for(|phase| {
                                matches!(phase, Finalization::Complete | Finalization::Failed)
                            })
                            .await
                            .is_ok_and(|phase| *phase == Finalization::Failed);
                        if failed_finalizer {
                            let _ = process.terminate_confirmed().await;
                        }
                        process.wait_backend_and_output().await;
                        process.output_drained_notify().notified().await;
                    };
                    tokio::pin!(settled);
                    tokio::select! {
                        () = &mut settled => {},
                        () = owner.closing.cancelled() => {
                            // Acknowledgment retains the backend's existing meaning,
                            // not proof about descendants that escaped its custody.
                            if let Err(error) = process.terminate_confirmed().await {
                                tracing::error!(%error, "process settlement termination failed");
                            }
                            process.wait_backend_and_output().await;
                        }
                    }
                    if !owner.closing.is_cancelled() {
                        owner
                            .processes
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&key(&process));
                    }
                }
            });
            rx
        };
        result
            .await
            .unwrap_or_else(|error| Err(UnifiedExecError::process_failed(error.to_string())))
    }

    pub(super) fn register(&self, process_id: i32, process: Arc<UnifiedExecProcess>) {
        let Some(hold) = self
            .launches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&process_id)
            .cloned()
        else {
            return;
        };
        let mut entries = self
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.insert(
            key(&process),
            OwnedProcess {
                process,
                hold,
                finalization: watch::channel(Finalization::Initial).0,
            },
        );
    }

    pub(super) fn background(&self, process: &Arc<UnifiedExecProcess>) {
        if let Some(entry) = self
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key(process))
        {
            entry.finalization.send_replace(Finalization::Background);
        }
    }

    pub(super) fn finalized(&self, process: &Arc<UnifiedExecProcess>) {
        if let Some(entry) = self
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key(process))
        {
            entry.finalization.send_if_modified(|phase| {
                if *phase == Finalization::Failed {
                    return false;
                }
                *phase = Finalization::Complete;
                true
            });
        }
    }

    pub(super) fn spawn_finalizer(
        self: &Arc<Self>,
        process: Arc<UnifiedExecProcess>,
        future: impl Future<Output = ()> + Send + 'static,
    ) {
        self.spawn_observer(process, future, ObserverKind::End);
    }

    pub(super) fn spawn_streamer(
        self: &Arc<Self>,
        process: Arc<UnifiedExecProcess>,
        future: impl Future<Output = ()> + Send + 'static,
    ) {
        self.spawn_observer(process, future, ObserverKind::Stream);
    }

    fn spawn_observer(
        self: &Arc<Self>,
        process: Arc<UnifiedExecProcess>,
        future: impl Future<Output = ()> + Send + 'static,
        kind: ObserverKind,
    ) {
        let owner = Arc::clone(self);
        self.tasks.spawn(async move {
            if std::panic::AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .is_err()
            {
                tracing::error!("process observer panicked; reconciling retained custody");
                if let ObserverKind::Stream = kind {
                    process.output_drained_notify().fail();
                }
                if let Some(entry) = owner
                    .processes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&key(&process))
                {
                    entry.finalization.send_replace(Finalization::Failed);
                }
            }
        });
    }

    pub(super) async fn shutdown(&self) {
        let processes = {
            let entries = self
                .processes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.closing.cancel();
            self.tasks.close();
            entries
                .values()
                .map(|entry| Arc::clone(&entry.process))
                .collect::<Vec<_>>()
        };
        for process in processes {
            let _ = process.terminate_confirmed().await;
        }
        self.tasks.wait().await;
        self.processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}
