//! Admission and automatic-close share one phase, with no async phase-lock edge.
//!
//! Registrars retain `SessionState` from their open check through insertion. Close
//! takes that same lock only after nonblocking turn/admission/queue probes. Runtime
//! holds linearize against the final seal under the watch sender's mutation lock.

use super::session::Session;
use crate::CodexThread;
use crate::runtime_holds::RuntimeActivity;
use crate::runtime_holds::RuntimeHolds;
use crate::state::ActiveTurn;
use crate::state::TurnState;
use std::sync::Arc;
use tokio::sync::Mutex;

impl Session {
    pub(crate) async fn begin_explicit_close(&self) {
        self.state.lock().await.runtime_holds.close();
    }

    pub(crate) fn reserve_runtime_turn(&self) -> Option<ActiveTurn> {
        Some(ActiveTurn {
            task: None,
            finishing: None,
            turn_state: Arc::new(Mutex::new(TurnState::default())),
            runtime_hold: Arc::new(self.runtime_holds.acquire()?),
        })
    }

    /// Published before the terminal event, retained until the executing future
    /// actually drops its last reservation token, not merely its task controller.
    pub(crate) async fn publish_terminal_settlement(&self, turn_state: &Arc<Mutex<TurnState>>) {
        let active = self.active_turn.lock().await;
        let Some(turn) = active
            .as_ref()
            .filter(|turn| Arc::ptr_eq(&turn.turn_state, turn_state))
        else {
            return;
        };
        *self
            .terminal_settlement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(turn.runtime_hold.settlement());
    }

    pub(crate) async fn settle_published_terminal_task(&self) {
        let settlement = self
            .terminal_settlement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(settlement) = settlement {
            settlement.cancelled().await;
        }
    }

    pub(crate) fn seal_idle_runtime(&self, revision: u64) -> crate::runtime_holds::IdleSeal {
        use crate::runtime_holds::IdleSeal;
        let Ok(_admission) = self.mcp_extension_admission.try_acquire() else {
            return IdleSeal::Contended;
        };
        let Ok(_publication) = self.terminal_publication.try_lock() else {
            return IdleSeal::Contended;
        };
        let Ok(active) = self.active_turn.try_lock() else {
            return IdleSeal::Contended;
        };
        if active.is_some() || self.has_outstanding_durable_sleep() {
            return IdleSeal::Busy;
        }
        match self.input_queue.can_prove_mailbox_empty() {
            None => return IdleSeal::Contended,
            Some(false) => return IdleSeal::Busy,
            Some(true) => {}
        }
        let Ok(_state) = self.state.try_lock() else {
            return IdleSeal::Contended;
        };
        if self.runtime_holds.seal(revision) {
            IdleSeal::Sealed
        } else {
            IdleSeal::Busy
        }
    }
}

impl CodexThread {
    /// The owner belongs to this runtime, never merely to its durable thread ID.
    pub fn runtime_holds(&self) -> RuntimeHolds {
        self.session.runtime_holds.clone()
    }

    /// Observes automatic teardown's residency exclusion without initiating it.
    /// Closing is never made admissible by waiting; callers must re-resolve identity.
    pub async fn wait_for_idle_close(&self) {
        let _gate = self.residency_gate.read().await;
    }

    pub(crate) async fn capacity_idle_revision(&self) -> Option<u64> {
        // Only an already-published terminal task may be awaited. New work is
        // rechecked by sealing, never queued or retried by capacity admission.
        self.session.settle_published_terminal_task().await;
        match self.session.runtime_holds.activity() {
            RuntimeActivity::Quiet { revision, .. } => Some(revision),
            RuntimeActivity::Held { .. } | RuntimeActivity::Closing => None,
        }
    }
}
