//! Admission and automatic-close share one phase, with no async phase-lock edge.
//!
//! Registrars retain `SessionState` from their open check through insertion. Close
//! takes that same lock only after nonblocking turn/admission/queue probes. Runtime
//! holds linearize against the final seal under the watch sender's mutation lock.

use super::session::Session;
use crate::CodexThread;
use crate::runtime_holds::RuntimeHolds;
use crate::state::ActiveTurn;
use crate::state::TurnState;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) enum IdleMailboxPolicy {
    Empty,
    RetainQueued,
}

impl Session {
    pub(crate) async fn begin_explicit_close(&self) {
        self.state.lock().await.runtime_holds.close();
    }

    pub(crate) fn reserve_runtime_turn(&self) -> Option<ActiveTurn> {
        Some(ActiveTurn {
            task: None,
            turn_id: None,
            sampling_interrupt: None,
            finishing: None,
            turn_state: Arc::new(Mutex::new(TurnState::default())),
            runtime_hold: Arc::new(self.runtime_holds.acquire()?),
            admission: None,
        })
    }

    /// Keeps an admitted start's host permit until its turn ends, so a host drain
    /// still counts the turn before observing its first event.
    pub(crate) async fn retain_turn_admission(
        &self,
        turn_id: &str,
        admission: Option<Box<dyn Send + Sync>>,
    ) {
        let Some(admission) = admission else {
            return;
        };
        if let Some(turn) = self.active_turn.lock().await.as_mut()
            && turn
                .task
                .as_ref()
                .is_some_and(|task| task.turn_context.sub_id == turn_id)
        {
            turn.admission = Some(admission);
        }
    }

    pub(crate) fn seal_idle_runtime(
        &self,
        revision: u64,
        mailbox: &IdleMailboxPolicy,
    ) -> crate::runtime_holds::IdleSeal {
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
        let mailbox_ready = match mailbox {
            IdleMailboxPolicy::Empty => self.input_queue.can_prove_mailbox_empty(),
            IdleMailboxPolicy::RetainQueued => self.input_queue.can_prove_mailbox_retainable(),
        };
        match mailbox_ready {
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
}
