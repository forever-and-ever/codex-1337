//! Admit notify before its callback completes, using the actual receiving turn.

use super::Session;
use super::TurnInput;
use codex_history::ResponseItemEnvelope;
use codex_protocol::code_mode::CodeModeOwner;
use codex_protocol::models::ResponseItem;
use std::sync::Arc;

impl Session {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "preserve inject_if_running atomic admission"
    )]
    pub(crate) async fn inject_code_mode_notification(
        &self,
        owner: CodeModeOwner,
        item: ResponseItemEnvelope,
    ) -> Result<(), Box<ResponseItemEnvelope>> {
        let ResponseItem::CustomToolCallOutput { call_id, .. } = &item.item else {
            unreachable!("Code Mode notify constructs its custom output payload before injection");
        };
        let mut active = self.active_turn.lock().await;
        let Some(active) = active.as_mut() else {
            return Err(Box::new(item));
        };
        // Registry admission is synchronous and does not invoke callbacks. As with
        // inject_if_running, an ActiveTurn with task=None still accepts this input.
        let lease = self.services.code_mode_service.begin_notification_capture(
            owner,
            call_id.clone(),
            Arc::downgrade(&active.turn_state),
        );
        let pending = if let Some(task) = &active.task {
            // Callback completion gates the runtime result. Admit notify to the
            // same history owner now, rather than deferring it behind that result.
            self.record_annotated_conversation_items(
                &task.turn_context,
                &task.turn_context.capture_current_model_info(),
                vec![item],
            )
            .await;
            vec![TurnInput::CodeModeNotification]
        } else {
            // Task completion already detached its context; its retained input
            // queue remains the receiving owner during terminal cleanup.
            vec![TurnInput::ResponseItem(item)]
        };
        self.input_queue
            .extend_pending_input_and_notify_for_turn_state(active.turn_state.as_ref(), pending)
            .await;
        lease.accepted();
        Ok(())
    }
}
