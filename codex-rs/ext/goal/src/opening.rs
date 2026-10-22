//! Idle goal continuation openings: a reminder, or the user's recorded instruction.

use crate::steering::continuation_steering_item;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_core::context::UserGoalUpdate;
use codex_protocol::protocol::ThreadGoal;

/// What opens an idle goal continuation turn.
#[derive(Clone, Copy, Debug)]
pub enum ContinuationOpening {
    /// A continuation reminder restates the objective and budget.
    Reminder,
    /// The user's goal instruction, already in history, states the whole objective.
    UserInstruction,
}

impl ContinuationOpening {
    /// The opening after a user goal mutation that recorded `instruction`, if any.
    pub fn after(instruction: Option<&UserGoalUpdate>) -> Self {
        if instruction.is_some_and(UserGoalUpdate::states_objective) {
            Self::UserInstruction
        } else {
            Self::Reminder
        }
    }

    pub(crate) fn turn_input(self, goal: &ThreadGoal) -> TurnInputRequest {
        match self {
            // The instruction states no budget, so a budgeted goal still opens on the reminder.
            // Empty automatic input resumes sampling without a new message.
            Self::UserInstruction if goal.token_budget.is_none() => {
                TurnInputRequest::user_input(Vec::new())
            }
            Self::UserInstruction | Self::Reminder => {
                TurnInputRequest::new(TurnInput::ResponseItem(continuation_steering_item(goal)))
            }
        }
    }
}
