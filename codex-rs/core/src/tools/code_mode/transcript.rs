//! Append-only execution facts for human transcript projection, never model history.

use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use codex_protocol::code_mode::CodeModeEvent;
use codex_protocol::code_mode::CodeModeItem;
use codex_protocol::code_mode::CodeModeOwner;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;

pub(super) async fn record(
    session: &Session,
    turn: &TurnContext,
    owner: CodeModeOwner,
    event: CodeModeEvent,
    timestamp_ms: i64,
) {
    let exec_call_id = &owner.exec_call_id;
    let suffix = match &event {
        CodeModeEvent::Started { call_id, .. } => format!("start:{call_id}"),
        CodeModeEvent::Allocated => "allocation".into(),
        CodeModeEvent::ToolCall { call_id } => format!("tool:{call_id}"),
        CodeModeEvent::WaitStarted { call_id } => format!("wait-start:{call_id}"),
        CodeModeEvent::WaitCompleted { call_id } => format!("wait-end:{call_id}"),
        CodeModeEvent::Finished { .. } => "end".into(),
    };
    session
        .send_event_with_owner(
            turn,
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: session.thread_id,
                turn_id: turn.sub_id.clone(),
                item: TurnItem::CodeMode(CodeModeItem {
                    id: format!("code-mode:{exec_call_id}:{suffix}"),
                    cell_id: owner.cell_id.clone(),
                    exec_call_id: exec_call_id.to_owned(),
                    created_at: timestamp_ms.div_euclid(1_000),
                    event,
                }),
                started_at_ms: None,
                completed_at_ms: timestamp_ms,
            }),
            Some(owner),
        )
        .await;
}
