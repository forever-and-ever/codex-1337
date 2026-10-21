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
        if self
            .turn_lifecycle
            .rendered_completion_turn_ids
            .contains(&turn.id)
        {
            return None;
        }
        let elapsed_seconds = turn
            .duration_ms
            .and_then(|duration| u64::try_from(duration).ok())
            .map(|duration| duration / 1_000)
            .or_else(|| {
                if replay_kind.is_none() {
                    self.bottom_pane
                        .status_elapsed()
                        .map(|elapsed| elapsed.as_secs())
                } else {
                    None
                }
            });
        let completed_at = turn
            .completed_at
            .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, /*nsecs*/ 0))
            .map(|timestamp| timestamp.with_timezone(&Local))
            .or_else(|| replay_kind.is_none().then(Local::now));
        if completed_at.is_none() && elapsed_seconds.is_none() {
            return None;
        }
        self.turn_lifecycle
            .rendered_completion_turn_ids
            .insert(turn.id.clone());
        let cell = history_cell::FinalMessageSeparator::new(
            elapsed_seconds,
            /*runtime_metrics*/ None,
        );
        Some(match completed_at {
            Some(completed_at) => {
                cell.with_completed_at(completed_at, crate::clock_format::ClockFormat::system())
            }
            None => cell,
        })
    }
}
