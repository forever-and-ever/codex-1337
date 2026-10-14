//! Capture input before dispatch normalizes a model-authored invocation.

use super::Session;
use super::TurnContext;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::tool_input::ToolInput;

impl Session {
    pub(super) async fn capture_tool_input(&self, turn: &TurnContext, item: &ResponseItem) {
        if let Some(input) = ToolInput::capture(item) {
            self.emit_turn_item_completed(turn, TurnItem::ToolInput(input))
                .await;
        }
    }
}
