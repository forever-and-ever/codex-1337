//! Preflight owns only a proposed checkpoint: the current worker and its input remain live.
//! A private reply channel makes a timed-out probe unable to admit a later request.

use super::control::Response;
use super::deadline::Deadline;
use super::probe;
use super::retained::checkpoint_version;
use super::worker_process::Slot;
use serde_json::Value;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;
use tokio::sync::oneshot;

pub(super) enum Reload {
    Idle,
    Failed,
    Checking(Admission),
    Replacing {
        deadline: Deadline,
        idle_candidate: Option<PathBuf>,
    },
}

impl Reload {
    pub(super) fn idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub(super) fn replacing(&self) -> bool {
        matches!(self, Self::Replacing { .. })
    }

    pub(super) fn failed(&self) -> bool {
        matches!(
            self,
            Self::Failed
                | Self::Checking(Admission {
                    origin: Origin::Recovery,
                    ..
                })
        )
    }
}

pub(super) enum Origin {
    Worker {
        reply: oneshot::Sender<Response>,
        idle_candidate: Option<PathBuf>,
    },
    Recovery,
}

pub(super) struct Admission {
    pub(super) checkpoint: Value,
    pub(super) origin: Origin,
    pub(super) expires: Instant,
    pub(super) result: oneshot::Receiver<io::Result<PathBuf>>,
}

impl Admission {
    pub(super) fn new(
        checkpoint: Value,
        candidate: PathBuf,
        origin: Origin,
        slots: &Arc<AtomicUsize>,
    ) -> Self {
        let expires = Instant::now() + probe::BUDGET;
        let version = checkpoint_version(&checkpoint);
        let (reply, result) = oneshot::channel();
        if let Some(slot) = Slot::reserve(slots) {
            // Even canonicalization and spawn can stall in the kernel. The host's
            // independent expiry rejects admission while this slot stays bounded.
            tokio::task::spawn_blocking(move || {
                let result = tokio::runtime::Handle::current()
                    .block_on(async { probe::admit(&candidate, version?).await });
                drop(slot);
                let _ = reply.send(result);
            });
        } else {
            let _ = reply.send(Err(io::Error::other("No UI probe slot available")));
        }
        Self {
            checkpoint,
            origin,
            expires,
            result,
        }
    }
}
