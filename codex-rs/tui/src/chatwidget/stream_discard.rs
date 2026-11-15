use super::AppEvent;
use super::ChatWidget;
use super::StatusIndicatorState;

impl ChatWidget {
    pub(super) fn discard_stream_items(&mut self, item_ids: Vec<String>) {
        let discarded = |id: Option<&String>| id.is_some_and(|id| item_ids.contains(id));
        if discarded(self.transcript.streaming_agent_item.as_ref()) {
            self.stream_controller = None;
            self.transcript.streaming_agent_item = None;
        }
        if discarded(self.transcript.streaming_plan_item.as_ref()) {
            self.plan_stream_controller = None;
            self.transcript.streaming_plan_item = None;
            self.transcript.plan_delta_buffer.clear();
            self.transcript.plan_item_active = false;
        }
        if discarded(self.status_state.reasoning_item_id.as_ref()) {
            self.reasoning_buffer.clear();
            self.reasoning_summary_parts.clear();
            self.reasoning_header = None;
            self.status_state.retry_status_header = Some(StatusIndicatorState::working().header);
            self.status_state.reasoning_item_id = None;
            self.status_state.reasoning_resume_turn_id = None;
        }
        self.clear_active_stream_tail();
        self.adaptive_chunking.reset();
        self.sync_active_stream_tail();
        self.app_event_tx
            .send(AppEvent::DiscardStreamItems(item_ids));
        if self.stream_controllers_idle() {
            self.app_event_tx.send(AppEvent::StopCommitAnimation);
        }
        self.request_redraw();
    }
}
