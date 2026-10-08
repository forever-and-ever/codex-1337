//! Attribute notify capture to its actual receiving input queue, not a stale worker turn.

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
            panic!("Code Mode notification requires its existing custom output payload");
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
        self.input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                active.turn_state.as_ref(),
                vec![TurnInput::ResponseItem(item)],
            )
            .await;
        lease.accepted();
        Ok(())
    }
}
