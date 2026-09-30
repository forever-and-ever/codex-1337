//! Rendering compatibility for unsolicited server-owned task messages.
pub(crate) use codex_app_server_protocol::delegation::DelegatedMessage;
use codex_app_server_protocol::{DynamicToolCallOutputContentItem, DynamicToolCallResponse};
use codex_protocol::models::FunctionCallOutputBody;
pub(crate) fn failure_response(message: impl Into<String>) -> DynamicToolCallResponse {
    DynamicToolCallResponse {
        content_items: vec![DynamicToolCallOutputContentItem::InputText {
            text: truncate(&message.into(), /*limit*/ 248),
        }],
        success: false,
    }
}
pub(crate) fn parse_delegated_tool_output(
    name: &str,
    namespace: Option<&str>,
    output: &FunctionCallOutputBody,
) -> Option<DelegatedMessage> {
    DelegatedMessage::from_tool_output(name, namespace, output)
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
