//! Tracks client runtime changes and turn starts admitted before a shutdown drain,
//! and the admitted turns Core has not yet finished.

use codex_app_server_protocol::JSONRPCErrorError;
use codex_extension_api::TurnStartAdmission;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use tokio::sync::watch;

use crate::error_code::server_draining_error;

/// Outstanding admission permits, by holder.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Admissions {
    /// Requests still preparing or submitting admitted work.
    pub(crate) requests: usize,
    /// Admitted turn starts; Core holds each permit until its turn ends.
    pub(crate) turns: usize,
}

impl Admissions {
    pub(crate) fn is_empty(self) -> bool {
        self.requests == 0 && self.turns == 0
    }
}

#[derive(Debug, Default)]
struct AdmissionState {
    closed: bool,
    active: Admissions,
}

#[derive(Clone, Debug)]
pub(crate) struct TurnAdmission {
    state: Arc<Mutex<AdmissionState>>,
    active_tx: watch::Sender<Admissions>,
}

impl Default for TurnAdmission {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(AdmissionState::default())),
            active_tx: watch::channel(Admissions::default()).0,
        }
    }
}

pub(crate) struct TurnPermit {
    admission: TurnAdmission,
    holder: Holder,
}

#[derive(Clone, Copy)]
enum Holder {
    Request,
    Turn,
}

impl Holder {
    fn count(self, active: &mut Admissions) -> &mut usize {
        match self {
            Self::Request => &mut active.requests,
            Self::Turn => &mut active.turns,
        }
    }
}

impl TurnAdmission {
    pub(crate) fn begin_drain(&self) {
        self.lock().closed = true;
    }

    /// Check quiescence and close admission atomically with respect to new work.
    #[cfg(unix)]
    pub(crate) fn begin_idle_drain(&self, running_turns: &watch::Receiver<usize>) -> bool {
        let mut state = self.lock();
        // Core holds an admitted start's permit until its turn ends, so a turn the
        // running count has yet to observe still holds admission. Read the running
        // count under this lock: an earlier snapshot can miss a request that acquires
        // and releases its permit before we close the gate.
        if !state.active.is_empty() || *running_turns.borrow() != 0 {
            return false;
        }
        state.closed = true;
        true
    }

    pub(crate) fn subscribe_active(&self) -> watch::Receiver<Admissions> {
        self.active_tx.subscribe()
    }

    // Admit and close take the same short lock. The permit keeps shutdown from
    // finishing while an earlier request is still preparing or submitting work.
    pub(crate) fn admit(&self) -> Result<TurnPermit, JSONRPCErrorError> {
        self.acquire(Holder::Request)
    }

    fn acquire(&self, holder: Holder) -> Result<TurnPermit, JSONRPCErrorError> {
        let mut state = self.lock();
        if state.closed {
            return Err(server_draining_error());
        }
        *holder.count(&mut state.active) += 1;
        self.active_tx.send_replace(state.active);
        Ok(TurnPermit {
            admission: self.clone(),
            holder,
        })
    }

    fn lock(&self) -> MutexGuard<'_, AdmissionState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl TurnStartAdmission for TurnAdmission {
    fn admit_turn_start(&self) -> Option<Box<dyn Send + Sync>> {
        self.acquire(Holder::Turn)
            .ok()
            .map(|permit| Box::new(permit) as Box<dyn Send + Sync>)
    }
}

impl Drop for TurnPermit {
    fn drop(&mut self) {
        let mut state = self.admission.lock();
        *self.holder.count(&mut state.active) -= 1;
        self.admission.active_tx.send_replace(state.active);
    }
}

#[cfg(test)]
#[path = "turn_admission_tests.rs"]
mod tests;
