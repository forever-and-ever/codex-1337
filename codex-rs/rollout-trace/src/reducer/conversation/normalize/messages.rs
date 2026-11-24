//! Messages wire blocks projected into the shared conversation reducer vocabulary.

use std::collections::HashSet;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_protocol::models::AnthropicSignedCompaction;
use serde_json::Value;

use super::NormalizedConversationItem;
use super::native_compaction_item;
use super::normalize_model_item;
use super::signed_payload_ref_part;
use crate::model::ConversationBody;
use crate::model::ConversationChannel;
use crate::model::ConversationItemKind;
use crate::model::ConversationPart;
use crate::model::ConversationRole;
use crate::model::NativeSignatureKind;
use crate::payload::RawPayloadRef;

pub(in super::super) fn normalize_messages_request(
    request: &Value,
    raw_payload: &RawPayloadRef,
    missing: &Value,
) -> Result<Vec<NormalizedConversationItem>> {
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .with_context(|| {
            format!(
                "Messages request payload {} did not contain array messages",
                raw_payload.raw_payload_id
            )
        })?;
    let mut items = Vec::new();
    let mut custom_calls = HashSet::new();
    if let Some(system) = request.get("system") {
        append_content(
            system,
            &missing["system"],
            "system",
            raw_payload,
            &mut custom_calls,
            &mut items,
        )?;
    }
    for (index, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .context("Messages request message had no role")?;
        if !matches!(role, "user" | "assistant") {
            bail!("Messages request contained unsupported message role");
        }
        let content = message
            .get("content")
            .context("Messages request message had no content")?;
        append_content(
            content,
            &missing["messages"][index]["content"],
            role,
            raw_payload,
            &mut custom_calls,
            &mut items,
        )?;
    }
    Ok(items)
}

fn append_content(
    content: &Value,
    missing: &Value,
    role: &str,
    raw_payload: &RawPayloadRef,
    custom_calls: &mut HashSet<String>,
    items: &mut Vec<NormalizedConversationItem>,
) -> Result<()> {
    if let Some(text) = content.as_str() {
        items.push(normalize_model_item(
            &mut serde_json::json!({
                "type": "message", "role": role, "content": [{"type": "text", "text": text}],
            }),
            raw_payload,
            &Value::Null,
        )?);
        return Ok(());
    }
    let blocks = content
        .as_array()
        .context("Messages content was neither a string nor an array")?;
    for (index, block) in blocks.iter().enumerate() {
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .context("Messages content block had no type")?;
        let block_missing = &missing[index];
        let (mut item, item_missing) = match kind {
            "compaction" => {
                let checkpoint: AnthropicSignedCompaction =
                    serde_json::from_value(block.clone())
                        .context("invalid signed Messages compaction block")?;
                items.push(native_compaction_item(&checkpoint, raw_payload));
                continue;
            }
            "tool_use" => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .context("Messages tool_use had no id")?;
                let input = block
                    .get("input")
                    .context("Messages tool_use had no input")?;
                // These envelopes are the native adapter's explicit function/custom contract.
                if let Some(input) = input
                    .as_object()
                    .filter(|input| input.len() == 1)
                    .and_then(|input| input.get("input"))
                    .and_then(Value::as_str)
                {
                    custom_calls.insert(id.to_string());
                    (
                        serde_json::json!({"type": "custom_tool_call", "call_id": id, "input": input}),
                        Value::Null,
                    )
                } else {
                    // Only the adapter's exact one-key object envelope can be unwrapped.
                    // Rejected/unknown native calls retain malformed input in full as raw JSON.
                    let arguments = input
                        .as_object()
                        .filter(|input| input.len() == 1)
                        .and_then(|input| input.get("arguments"))
                        .filter(|arguments| arguments.is_object())
                        .unwrap_or(input);
                    (
                        serde_json::json!({"type": "function_call", "call_id": id, "arguments": arguments}),
                        Value::Null,
                    )
                }
            }
            "tool_result" => {
                let id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .context("Messages tool_result had no tool_use_id")?;
                (
                    serde_json::json!({"type": if custom_calls.contains(id) { "custom_tool_call_output" } else { "function_call_output" },
                        "call_id": id, "output": block.get("content")}),
                    serde_json::json!({"output": block_missing["content"]}),
                )
            }
            "thinking" | "redacted_thinking" => {
                // Signed thinking is lazy evidence, not JSON that might print its signature.
                let (kind, signature) = if kind == "thinking" {
                    (
                        NativeSignatureKind::Thinking,
                        block.get("signature").and_then(Value::as_str),
                    )
                } else {
                    (
                        NativeSignatureKind::RedactedThinking,
                        block.get("data").and_then(Value::as_str),
                    )
                };
                let signature = signature.context("signed Messages thinking had no signature")?;
                let mut parts = Vec::new();
                if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                    parts.push(ConversationPart::Summary {
                        text: text.to_string(),
                    });
                }
                parts.push(signed_payload_ref_part(kind, signature, raw_payload));
                items.push(NormalizedConversationItem {
                    responses_item_id: None,
                    structured_output_hash: None,
                    role: ConversationRole::Assistant,
                    channel: Some(ConversationChannel::Analysis),
                    kind: ConversationItemKind::Reasoning,
                    agent_message: None,
                    body: ConversationBody { parts },
                    call_id: None,
                });
                continue;
            }
            _ => (
                serde_json::json!({"type": "message", "role": role, "content": [block]}),
                serde_json::json!({"content": [block_missing]}),
            ),
        };
        items.push(normalize_model_item(&mut item, raw_payload, &item_missing)?);
    }
    Ok(())
}
