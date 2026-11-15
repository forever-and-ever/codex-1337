//! Completion metadata for live and restored turns. Saved timestamps are authoritative;
//! only a live completion may fall back to the local clock. Labels are deduplicated per thread.

use super::*;

impl ChatWidget {
    pub(super) fn record_final_response(&self, turn: &codex_app_server_protocol::Turn) {
        let id = crate::transcript_view::dialogue::final_response(turn).or_else(|| {
            self.transcript
                .last_completed_agent_message
                .as_ref()
                .filter(|message| {
                    message.turn_id == turn.id
                        && matches!(
                            message.phase,
                            Some(codex_protocol::models::MessagePhase::FinalAnswer) | None
                        )
                })
                .map(|message| message.item_id.as_str())
        });
        if let (Some(thread_id), Some(id)) = (self.thread_id, id) {
            self.app_event_tx
                .send(crate::app_event::AppEvent::RecordFinalResponse {
                    thread_id,
                    item_id: id.to_string(),
                });
        }
    }

    pub(crate) fn completion_cell(
        &mut self,
        turn: &Turn,
        replay_kind: Option<ReplayKind>,
    ) -> Option<history_cell::FinalMessageSeparator> {
        let live = replay_kind.is_none();
        history_cell::FinalMessageSeparator::from_turn_once(
            turn,
            &mut self.turn_lifecycle.rendered_completion_turn_ids,
            self.bottom_pane.status_elapsed().filter(|_| live),
            live.then(Local::now),
        )
        .filter(|cell| self.local_settings.transcript_mode.is_owned() || cell.has_timing())
    }
}
