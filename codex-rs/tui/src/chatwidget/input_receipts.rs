//! Submission receipts update the canonical input owner; ambiguous delivery is never retried.

use super::*;

impl ChatWidget {
    pub(super) fn take_submitted_message(
        &mut self,
        client_id: &str,
    ) -> Option<(UserMessage, UserMessageHistoryRecord, UserMessageSource)> {
        if self
            .input_queue
            .turn
            .submitted()
            .is_some_and(|input| input.client_id == client_id)
        {
            let input = match std::mem::take(&mut self.input_queue.turn) {
                input_queue::UserTurnInput::AwaitingStart(
                    input_queue::PendingTurnStart::Submitted(input),
                )
                | input_queue::UserTurnInput::Started(input) => input,
                _ => unreachable!("submitted input was checked before taking its owner"),
            };
            Some((input.message, input.history, input.source))
        } else {
            self.input_queue
                .pending_steer
                .take_client(client_id)
                .map(|pending| (pending.user_message, pending.history_record, pending.source))
        }
    }

    #[cfg(test)]
    pub(crate) fn restore_rejected_user_message(&mut self, client_id: &str, message: String) {
        if let Some((draft, history, _)) = self.take_submitted_message(client_id) {
            self.restore_user_message_to_composer(user_message_for_restore(draft, &history));
        }
        self.add_error_message(message);
        self.refresh_pending_input_preview();
        self.update_task_running_state();
    }
}
