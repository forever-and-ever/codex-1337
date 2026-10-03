use codex_protocol::models::ContentItem;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ResponseInputItem;
use pretty_assertions::assert_eq;

use super::ToolOutput;
use super::response_input_to_code_mode_result;

#[test]
fn code_mode_mcp_result_keeps_fault_but_not_private_receipt() {
    let result = codex_protocol::mcp::CallToolResult {
        content: vec![
            serde_json::json!({"type":"text","text":"partial publication; do not retry"}),
        ],
        structured_content: None,
        is_error: Some(true),
        meta: Some(serde_json::json!({"codex/fileChanges":{"private":"source bytes"}})),
    };
    assert_eq!(
        result.code_mode_result(&crate::ToolPayload::Function {
            arguments: "{}".into()
        }),
        serde_json::json!({"content":[{"type":"text","text":"partial publication; do not retry"}],"isError":true})
    );
}

#[test]
fn code_mode_text_preserves_file_backed_image_id() {
    let response = ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputImage {
            image: ImageReference::File {
                file_id: "file_123".to_string(),
            },
            detail: None,
        }],
        phase: None,
    };

    assert_eq!(
        response_input_to_code_mode_result(response),
        serde_json::json!("file_123")
    );
}
