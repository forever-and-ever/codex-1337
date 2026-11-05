//! A pending-work launch may use only the reservation that selected its context.

use crate::session::TurnInput;
use crate::session::session::Session;
use crate::state::ActiveTurn;
use crate::state::TurnState;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::MutexGuard;

pub(crate) enum TaskAdmission {
    Unreserved(Vec<TurnInput>),
    Reserved {
        input: Vec<TurnInput>,
        turn_state: Arc<Mutex<TurnState>>,
    },
}

impl From<Vec<TurnInput>> for TaskAdmission {
    fn from(input: Vec<TurnInput>) -> Self {
        Self::Unreserved(input)
    }
}

impl TaskAdmission {
    pub(super) fn matches(&self, turn: &ActiveTurn) -> bool {
        match self {
            Self::Unreserved(_) => true,
            Self::Reserved { turn_state, .. } => Arc::ptr_eq(&turn.turn_state, turn_state),
        }
    }

    pub(super) async fn lock<'a>(
        &self,
        session: &'a Session,
    ) -> Option<MutexGuard<'a, Option<ActiveTurn>>> {
        let mut active = session.active_turn.lock().await;
        if session.runtime_holds.is_closing() {
            return None;
        }
        match self {
            Self::Unreserved(_) => {
                if active.is_none() {
                    *active = Some(session.reserve_runtime_turn()?);
                }
            }
            Self::Reserved { .. } => {
                if active
                    .as_ref()
                    .is_none_or(|turn| !self.matches(turn) || turn.task.is_some())
                {
                    return None;
                }
            }
        }
        Some(active)
    }

    pub(super) fn into_input(self) -> Vec<TurnInput> {
        match self {
            Self::Unreserved(input) | Self::Reserved { input, .. } => input,
        }
    }
}
