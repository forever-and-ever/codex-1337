//! Capture user-turn targets before asynchronous preparation or app dispatch.

use super::AppCommand;
use super::AppEvent;
use super::ChatWidget;
use super::CodexOpTarget;
use super::PARENT_OWNED_INPUT_MESSAGE;
use super::ThreadId;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) enum UserTurnDispatch {
    #[default]
    Frontend,
    Thread(ThreadId),
    Prepared,
}

impl UserTurnDispatch {
    pub(super) fn is_frontend(&self) -> bool {
        matches!(self, Self::Frontend)
    }

    pub(super) fn capture(self, thread_id: Option<ThreadId>) -> Self {
        match (self, thread_id) {
            (Self::Frontend, Some(thread_id)) => Self::Thread(thread_id),
            _ => self,
        }
    }
}

impl ChatWidget {
    pub(super) fn submit_app_command(&mut self, op: AppCommand) -> bool {
        if matches!(&op, AppCommand::UserTurn { .. }) {
            self.dispatch_user_turn(UserTurnDispatch::Frontend.capture(self.thread_id), op)
        } else {
            self.submit_op_with_target(op, None)
        }
    }

    pub(super) fn dispatch_user_turn(
        &mut self,
        dispatch: UserTurnDispatch,
        op: AppCommand,
    ) -> bool {
        match dispatch {
            UserTurnDispatch::Thread(thread_id) => self.submit_op_with_target(op, Some(thread_id)),
            UserTurnDispatch::Prepared => true,
            UserTurnDispatch::Frontend => {
                if matches!(self.codex_op_target, CodexOpTarget::Direct(_)) {
                    self.submit_op_with_target(op, None)
                } else {
                    self.add_error_message(
                        "This conversation is not ready to accept input.".into(),
                    );
                    false
                }
            }
        }
    }

    pub(super) fn submit_op_with_target(
        &mut self,
        op: AppCommand,
        thread_id: Option<ThreadId>,
    ) -> bool {
        if self.rejects_misalignment_policy_op(&op) {
            return false;
        }
        if self.blocks_direct_input
            && matches!(
                &op,
                AppCommand::UserTurn { .. }
                    | AppCommand::CancelPendingSteer { .. }
                    | AppCommand::Review { .. }
                    | AppCommand::Compact
            )
        {
            self.add_error_message(if self.external_writer_view {
                "This thread is open elsewhere. Close it there and retry resume to continue."
                    .to_string()
            } else {
                PARENT_OWNED_INPUT_MESSAGE.to_string()
            });
            return false;
        }
        self.prepare_local_op_submission(&op);
        if op.is_review() && !self.bottom_pane.is_task_running() {
            self.bottom_pane.set_task_running(/*running*/ true);
        }
        match &self.codex_op_target {
            CodexOpTarget::Direct(codex_op_tx) => {
                crate::session_log::log_outbound_op(&op);
                let is_review = op.is_review();
                if let Err(e) = codex_op_tx.send(op) {
                    tracing::error!("failed to submit op: {e}");
                    return false;
                }
                if is_review {
                    self.on_review_started();
                }
            }
            CodexOpTarget::AppEvent => match thread_id {
                Some(thread_id) => self
                    .app_event_tx
                    .send(AppEvent::SubmitThreadOp { thread_id, op }),
                None => self.app_event_tx.send(AppEvent::CodexOp(op)),
            },
        }
        true
    }
}
