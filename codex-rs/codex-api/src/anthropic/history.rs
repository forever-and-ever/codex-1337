use super::AnthropicTool;
use super::AnthropicToolIdentity;
use crate::ApiError;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ImageReference;
use codex_protocol::models::NativeCompaction;
use codex_protocol::models::NativeToolCall;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::plaintext_agent_message_content;
use codex_protocol::openai_models::InputModality;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Serialize)]
pub(super) struct Message {
    pub role: &'static str,
    pub content: Vec<Value>,
}

pub(super) struct History {
    pub system: Vec<Value>,
    pub messages: Vec<Message>,
}

#[derive(PartialEq)]
enum ResultKind {
    Function,
    Custom,
    Rejected,
}

pub(super) fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::InvalidPrompt {
        message: message.into(),
    }
}

pub(super) fn encode_history(
    input: &[ResponseItem],
    instructions: &str,
    tools: &[AnthropicTool],
    modalities: &[InputModality],
) -> Result<History, ApiError> {
    let mut history = History {
        system: Vec::new(),
        messages: Vec::new(),
    };
    if !instructions.is_empty() {
        history
            .system
            .push(json!({"type":"text", "text":instructions}));
    }
    // A result belongs to the immediately preceding assistant tool batch. Reordering
    // results or inventing missing results would change both semantics and cache keys.
    let mut calls = HashSet::new();
    let mut pending = HashMap::new();
    for item in input {
        let had_pending = !pending.is_empty();
        let (role, content) = match item {
            ResponseItem::AgentMessage {
                author,
                recipient,
                content,
                ..
            } => {
                if author.len() > 512 || recipient.len() > 512 {
                    return Err(invalid("Anthropic agent-message address exceeds 512 bytes"));
                }
                let text = plaintext_agent_message_content(content)
                    .ok_or_else(|| invalid("Anthropic cannot replay encrypted agent messages"))?;
                let author = serde_json::to_string(author)
                    .map_err(|err| invalid(format!("cannot encode agent-message author: {err}")))?;
                let recipient = serde_json::to_string(recipient).map_err(|err| {
                    invalid(format!("cannot encode agent-message recipient: {err}"))
                })?;
                (
                    "user",
                    vec![
                        json!({"type":"text", "text":format!("[Agent message from {author} to {recipient}]\n{text}")}),
                    ],
                )
            }
            ResponseItem::Message { role, content, .. } => {
                let blocks = content
                    .iter()
                    .map(|part| match part {
                        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                            Ok(json!({"type":"text", "text":text}))
                        }
                        ContentItem::InputImage { image, .. } => encode_image(image, modalities),
                        ContentItem::InputAudio { .. } => {
                            Err(invalid("Anthropic audio input is unsupported"))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                match role.as_str() {
                    "system" | "developer" if history.messages.is_empty() => {
                        if blocks.iter().any(|block| block["type"] != "text") {
                            return Err(invalid(
                                "Anthropic system instructions must contain only text",
                            ));
                        }
                        history.system.extend(blocks);
                        continue;
                    }
                    "system" | "developer" => {
                        if blocks.iter().any(|block| block["type"] != "text") {
                            return Err(invalid(
                                "Anthropic system instructions must contain only text",
                            ));
                        }
                        // Native system-message placement cannot represent arbitrary
                        // canonical updates. Keep every update at its original position
                        // across requests, without reordering a signed or cached prefix.
                        let reminders = blocks.into_iter().map(|block| {
                            let text = block["text"].as_str().ok_or_else(|| invalid("Anthropic instruction block must contain text"))?;
                            Ok(json!({"type":"text", "text":format!("<system-reminder>\n{text}\n</system-reminder>")}))
                        }).collect::<Result<Vec<_>, ApiError>>()?;
                        ("user", reminders)
                    }
                    "user" => ("user", blocks),
                    "assistant" => ("assistant", blocks),
                    _ => {
                        return Err(invalid(format!(
                            "unsupported Anthropic message role: {role}"
                        )));
                    }
                }
            }
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                encrypted_function_args,
                call_id,
                ..
            } => {
                if encrypted_function_args.is_some() {
                    return Err(invalid(
                        "Anthropic cannot replay encrypted function arguments",
                    ));
                }
                let identity = AnthropicToolIdentity::Function {
                    name: name.clone(),
                    namespace: namespace.clone(),
                };
                let input: Value = serde_json::from_str(arguments).map_err(|_| {
                    invalid(format!("invalid JSON arguments for Anthropic tool {name}"))
                })?;
                if !input.is_object() {
                    return Err(invalid(format!(
                        "Anthropic tool {name} requires object arguments"
                    )));
                }
                let alias = history_alias(tools, &identity)?;
                if !calls.insert(call_id.as_str()) {
                    return Err(invalid("duplicate Anthropic tool call ID"));
                }
                pending.insert(call_id.as_str(), ResultKind::Function);
                (
                    "assistant",
                    vec![
                        json!({"type":"tool_use", "id":call_id, "name":alias, "input":{"arguments":input}}),
                    ],
                )
            }
            ResponseItem::CustomToolCall {
                name,
                namespace,
                input,
                call_id,
                ..
            } => {
                let identity = AnthropicToolIdentity::Custom {
                    name: name.clone(),
                    namespace: namespace.clone(),
                };
                let alias = history_alias(tools, &identity)?;
                if !calls.insert(call_id.as_str()) {
                    return Err(invalid("duplicate Anthropic tool call ID"));
                }
                pending.insert(call_id.as_str(), ResultKind::Custom);
                (
                    "assistant",
                    vec![
                        json!({"type":"tool_use", "id":call_id, "name":alias, "input":{"input":input}}),
                    ],
                )
            }
            ResponseItem::RejectedToolCall {
                call:
                    NativeToolCall::Anthropic {
                        call_id,
                        name,
                        input,
                    },
                ..
            } => {
                if call_id.is_empty() || name.is_empty() || !input.is_object() {
                    return Err(invalid("invalid rejected Anthropic tool call"));
                }
                if !calls.insert(call_id.as_str()) {
                    return Err(invalid("duplicate Anthropic tool call ID"));
                }
                pending.insert(call_id.as_str(), ResultKind::Rejected);
                (
                    "assistant",
                    vec![json!({"type":"tool_use", "id":call_id, "name":name, "input":input})],
                )
            }
            ResponseItem::FunctionCallOutput {
                call_id: None,
                name: Some(name),
                namespace,
                output,
                ..
            } => {
                // Host-delivered notifications have no originating tool call. Preserve
                // them as attributed input, not an invented tool_use/tool_result pair.
                if name.is_empty()
                    || name.len() > 512
                    || namespace.as_ref().is_some_and(|value| value.len() > 512)
                {
                    return Err(invalid("Anthropic tool notification has an invalid name"));
                }
                let source = match namespace.as_deref().filter(|value| !value.is_empty()) {
                    Some(namespace) => format!("{namespace}.{name}"),
                    None => name.clone(),
                };
                let label = if output.success == Some(false) {
                    "Failed tool notification"
                } else {
                    "Tool notification"
                };
                let mut content = vec![
                    json!({"type":"text", "text":format!("[{label} from {}]", json!(source))}),
                ];
                content.extend(encode_result_blocks(output, modalities)?);
                ("user", content)
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                let call_id = call_id
                    .as_deref()
                    .ok_or_else(|| invalid("Anthropic tool result has no call ID"))?;
                let kind = pending
                    .remove(call_id)
                    .ok_or_else(|| invalid("unpaired Anthropic tool result"))?;
                if kind == ResultKind::Custom {
                    return Err(invalid("function result paired with custom Anthropic tool"));
                }
                let mut result = encode_result(call_id, output, modalities)?;
                if kind == ResultKind::Rejected {
                    // FunctionCallOutputPayload's success hint is not serialized.
                    // The durable rejected call remains authoritative on resume.
                    result["is_error"] = json!(true);
                }
                ("user", vec![result])
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                let kind = pending
                    .remove(call_id.as_str())
                    .ok_or_else(|| invalid("unpaired Anthropic custom tool result"))?;
                if kind != ResultKind::Custom {
                    return Err(invalid("custom result paired with function Anthropic tool"));
                }
                ("user", vec![encode_result(call_id, output, modalities)?])
            }
            ResponseItem::Reasoning {
                summary,
                content,
                encrypted_content,
                ..
            } => {
                let signature = encrypted_content.as_deref().ok_or_else(|| {
                    invalid("Anthropic thinking replay requires its exact signature")
                })?;
                if signature.is_empty() {
                    return Err(invalid("Anthropic thinking replay signature is empty"));
                }
                let block = if let Some(data) = signature.strip_prefix("claude-redacted-thinking:")
                {
                    if !summary.is_empty()
                        || content.as_ref().is_some_and(|content| !content.is_empty())
                    {
                        return Err(invalid(
                            "redacted Anthropic thinking contains readable content",
                        ));
                    }
                    json!({"type":"redacted_thinking", "data":data})
                } else {
                    if content.as_ref().is_some_and(|content| !content.is_empty()) {
                        return Err(invalid("ambiguous Anthropic thinking content and summary"));
                    }
                    let thinking = summary
                        .iter()
                        .map(|part| match part {
                            ReasoningItemReasoningSummary::SummaryText { text } => text.as_str(),
                        })
                        .collect::<String>();
                    json!({"type":"thinking", "thinking":thinking, "signature":signature})
                };
                ("assistant", vec![block])
            }
            ResponseItem::NativeCompaction {
                compaction: NativeCompaction::Anthropic(compaction),
                ..
            } => (
                "assistant",
                vec![
                    json!({"type":"compaction", "content":compaction.content(), "signature":compaction.signature()}),
                ],
            ),
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::ConfigurationUpdate { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other => {
                return Err(invalid(
                    "history item has no supported native Anthropic representation",
                ));
            }
        };
        if content.is_empty() {
            return Err(invalid("empty Anthropic message"));
        }
        let is_result = content
            .first()
            .is_some_and(|block| block["type"] == "tool_result");
        let last_role = history.messages.last().map(|message| message.role);
        if had_pending && role != "assistant" && !is_result {
            return Err(invalid(
                "Anthropic user message interrupts unresolved tool calls",
            ));
        }
        if had_pending && role == "assistant" && last_role == Some("user") {
            return Err(invalid(
                "Anthropic assistant interrupts incomplete tool result batch",
            ));
        }
        if let Some(message) = history.messages.last_mut()
            && message.role == role
        {
            message.content.extend(content);
        } else {
            history.messages.push(Message { role, content });
        }
    }
    if !pending.is_empty() {
        return Err(invalid("Anthropic history ends with unresolved tool calls"));
    }
    if history.messages.is_empty() {
        return Err(invalid(
            "Anthropic history contains no conversation messages",
        ));
    }
    Ok(history)
}

fn history_alias(
    tools: &[AnthropicTool],
    identity: &AnthropicToolIdentity,
) -> Result<String, ApiError> {
    let declared = tools.iter().find(|tool| match (&tool.identity, identity) {
        (
            AnthropicToolIdentity::Function {
                name: a,
                namespace: an,
            },
            AnthropicToolIdentity::Function {
                name: b,
                namespace: bn,
            },
        )
        | (
            AnthropicToolIdentity::Custom {
                name: a,
                namespace: an,
            },
            AnthropicToolIdentity::Custom {
                name: b,
                namespace: bn,
            },
        ) => {
            a == b
                && an
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .unwrap_or("functions")
                    == bn
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .unwrap_or("functions")
        }
        _ => false,
    });
    match declared {
        Some(tool) => Ok(tool.name.clone()),
        None => identity.wire_alias(),
    }
}

fn encode_image(image: &ImageReference, modalities: &[InputModality]) -> Result<Value, ApiError> {
    if !modalities.contains(&InputModality::Image) {
        return Err(invalid("selected Anthropic model does not accept images"));
    }
    let ImageReference::Inline { image_url } = image else {
        return Err(invalid("Anthropic cannot use an OpenAI image file ID"));
    };
    if let Some(data) = image_url.strip_prefix("data:") {
        let (media_type, data) = data
            .split_once(";base64,")
            .ok_or_else(|| invalid("Anthropic inline image requires base64 data"))?;
        if !matches!(
            media_type,
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ) || data.is_empty()
        {
            return Err(invalid(
                "unsupported Anthropic inline image media type or empty data",
            ));
        }
        Ok(
            json!({"type":"image", "source":{"type":"base64", "media_type":media_type, "data":data}}),
        )
    } else if image_url.starts_with("https://") || image_url.starts_with("http://") {
        Ok(json!({"type":"image", "source":{"type":"url", "url":image_url}}))
    } else {
        Err(invalid("unsupported Anthropic image URL"))
    }
}

fn encode_result(
    call_id: &str,
    output: &FunctionCallOutputPayload,
    modalities: &[InputModality],
) -> Result<Value, ApiError> {
    let content = match &output.body {
        FunctionCallOutputBody::Text(text) => Value::String(text.clone()),
        FunctionCallOutputBody::ContentItems(_) => {
            Value::Array(encode_result_blocks(output, modalities)?)
        }
    };
    Ok(
        json!({"type":"tool_result", "tool_use_id":call_id, "content":content, "is_error":output.success == Some(false)}),
    )
}

fn encode_result_blocks(
    output: &FunctionCallOutputPayload,
    modalities: &[InputModality],
) -> Result<Vec<Value>, ApiError> {
    match &output.body {
        FunctionCallOutputBody::Text(text) => Ok(vec![json!({"type":"text", "text":text})]),
        FunctionCallOutputBody::ContentItems(items) => items
            .iter()
            .map(|part| match part {
                FunctionCallOutputContentItem::InputText { text } => {
                    Ok(json!({"type":"text", "text":text}))
                }
                FunctionCallOutputContentItem::InputImage { image, .. } => {
                    encode_image(image, modalities)
                }
                FunctionCallOutputContentItem::InputAudio { .. } => {
                    Err(invalid("Anthropic audio tool results are unsupported"))
                }
                FunctionCallOutputContentItem::EncryptedContent { .. } => {
                    Err(invalid("Anthropic cannot replay encrypted tool results"))
                }
            })
            .collect(),
    }
}
