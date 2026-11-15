//! A completed response is addressed to its item, not whichever thread is active.

use crate::history_cell::HistoryCell;
use codex_protocol::ThreadId;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use uuid::Uuid;

#[derive(Debug)]
pub(crate) struct PreparedAssistant {
    pub(crate) markdown: String,
    pub(crate) source: String,
    pub(crate) branch_cwd: Option<std::path::PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PreparationToken(Uuid);

impl PreparationToken {
    // A ready non-response slot uses the existing wake-only receipt path.
    pub(crate) const WAKE: Self = Self(Uuid::nil());
}

#[derive(Debug)]
pub(crate) struct PreparedResponse {
    pub(crate) assistant: Option<PreparedAssistant>,
    pub(crate) cell: Arc<dyn HistoryCell>,
}

#[derive(Debug)]
enum Publication {
    Pending,
    Prepared(PreparedResponse),
    Published,
}

/// Shared custody survives both queued parking and off-thread completion.
#[derive(Clone, Debug)]
pub(crate) struct PreparedStream {
    pub(crate) thread_id: ThreadId,
    pub(crate) item_id: String,
    token: PreparationToken,
    publication: Arc<Mutex<Publication>>,
}

impl PreparedStream {
    pub(crate) fn pending(thread_id: ThreadId, item_id: String) -> Self {
        Self {
            thread_id,
            item_id,
            token: PreparationToken(Uuid::new_v4()),
            publication: Arc::new(Mutex::new(Publication::Pending)),
        }
    }

    pub(crate) fn same_origin(&self, other: &Self) -> bool {
        self.thread_id == other.thread_id
            && self.item_id == other.item_id
            && self.token == other.token
    }

    pub(crate) fn event_token(&self) -> PreparationToken {
        self.token
    }

    pub(crate) fn complete(
        &self,
        assistant: Option<PreparedAssistant>,
        cell: Arc<dyn HistoryCell>,
    ) {
        let mut publication = self
            .publication
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if matches!(*publication, Publication::Pending) {
            *publication = Publication::Prepared(PreparedResponse { assistant, cell });
        }
    }

    pub(crate) fn take_ready(&self) -> Option<PreparedResponse> {
        let mut publication = self
            .publication
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *publication, Publication::Published) {
            Publication::Prepared(response) => Some(response),
            previous => {
                *publication = previous;
                None
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use codex_protocol::ThreadId;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::PoisonError;
    use std::sync::Weak;
    use std::sync::mpsc;
    use tokio::sync::Notify;

    struct Gate {
        entered: Notify,
        release: Mutex<mpsc::Receiver<()>>,
    }

    static GATES: Mutex<Vec<(ThreadId, String, Weak<Gate>)>> = Mutex::new(Vec::new());

    pub(crate) struct Hold {
        thread_id: ThreadId,
        item_id: String,
        gate: Arc<Gate>,
        _release: mpsc::Sender<()>,
    }

    impl Hold {
        pub(crate) async fn entered(&self) {
            self.gate.entered.notified().await;
        }
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            GATES.lock().unwrap_or_else(PoisonError::into_inner).retain(
                |(thread_id, item_id, _)| *thread_id != self.thread_id || *item_id != self.item_id,
            );
        }
    }

    pub(crate) fn hold(thread_id: ThreadId, item_id: &str) -> Hold {
        let (release, receiver) = mpsc::channel();
        let gate = Arc::new(Gate {
            entered: Notify::new(),
            release: Mutex::new(receiver),
        });
        GATES.lock().unwrap_or_else(PoisonError::into_inner).push((
            thread_id,
            item_id.to_owned(),
            Arc::downgrade(&gate),
        ));
        Hold {
            thread_id,
            item_id: item_id.to_owned(),
            gate,
            _release: release,
        }
    }

    pub(crate) fn wait(thread_id: ThreadId, item_id: &str) {
        let gate = GATES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(thread, item, _)| *thread == thread_id && item == item_id)
            .and_then(|(_, _, gate)| gate.upgrade());
        if let Some(gate) = gate {
            gate.entered.notify_one();
            let _ = gate
                .release
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv();
        }
    }
}
