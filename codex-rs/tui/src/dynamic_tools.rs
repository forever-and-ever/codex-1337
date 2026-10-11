//! Rendering compatibility for unsolicited server-owned task messages.
use codex_app_server_protocol::DynamicToolCallOutputContentItem;
use codex_app_server_protocol::DynamicToolCallResponse;
pub(crate) use codex_app_server_protocol::delegation::DelegatedMessage;
use codex_protocol::models::FunctionCallOutputBody;
pub(crate) fn failure_response(message: impl Into<String>) -> DynamicToolCallResponse {
    DynamicToolCallResponse {
        content_items: vec![DynamicToolCallOutputContentItem::InputText {
            text: truncate(&message.into(), /*limit*/ 248),
        }],
        success: false,
    }
}
pub(crate) fn unsolicited_tool_output(
    call_id: Option<&str>,
    name: &str,
    namespace: Option<&str>,
    output: &FunctionCallOutputBody,
) -> Option<Box<dyn crate::history_cell::HistoryCell>> {
    if let Some(message) = DelegatedMessage::from_tool_output(name, namespace, output) {
        return Some(Box::new(crate::history_cell::DelegatedMessageCell(message)));
    }
    if call_id.is_none() {
        codex_app_server_protocol::alarm_sounding::AlarmSounding::from_tool_output(
            name, namespace, output,
        )
        .map(|alarm| {
            Box::new(crate::history_cell::AlarmSoundingCell(alarm))
                as Box<dyn crate::history_cell::HistoryCell>
        })
    } else {
        None
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    if limit == 0 {
        return String::new();
    }
    format!("{}…", text.chars().take(limit - 1).collect::<String>())
}
