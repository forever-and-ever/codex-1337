use super::AnthropicTool;
use super::AnthropicToolIdentity;
use super::tool_schema::function_arguments;
use super::usage::Usage;
use crate::ApiError;
use crate::ResponseEvent;
use codex_protocol::ResponseItemId;
use codex_protocol::ResponseUsageMetadata;
use codex_protocol::models::AnthropicSignedCompaction;
use codex_protocol::models::ContentItem;
use codex_protocol::models::NativeCompaction;
use codex_protocol::models::NativeToolCall;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

const MAX_BLOCKS: usize = 1024;

/// Interprets the native completion disposition after upstream usage is recorded.
pub fn anthropic_context_window_exceeded(metadata: &ResponseUsageMetadata) -> bool {
    matches!(
        anthropic_stop_reason(metadata),
        Some(AnthropicStopReason::ModelContextWindowExceeded)
    )
}

/// Reports native output truncation after upstream usage is recorded.
pub fn anthropic_output_limit_exceeded(metadata: &ResponseUsageMetadata) -> bool {
    matches!(
        anthropic_stop_reason(metadata),
        Some(AnthropicStopReason::MaxTokens)
    )
}

/// Returns native termination only for valid Anthropic completion metadata.
pub fn anthropic_stop_reason(metadata: &ResponseUsageMetadata) -> Option<AnthropicStopReason> {
    let raw = metadata.metadata.as_ref()?;
    if raw.get("provider").and_then(Value::as_str) != Some("anthropic") {
        return None;
    }
    AnthropicStopReason::deserialize(raw.get("stop_reason")?).ok()
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Frame {
    MessageStart { message: Message },
    ContentBlockStart { index: usize, content_block: Block },
    ContentBlockDelta { index: usize, delta: Delta },
    ContentBlockStop { index: usize },
    MessageDelta { delta: MessageDelta, usage: Value },
    MessageStop,
    Ping,
    Error { error: NativeError },
}

#[derive(Deserialize)]
struct Message {
    id: String,
    model: Option<String>,
    usage: Value,
}

#[derive(Deserialize)]
struct NativeError {
    r#type: String,
    message: String,
}

#[derive(Deserialize)]
struct MessageDelta {
    stop_reason: Option<AnthropicStopReason>,
}

/// Native Messages termination, without inferring completion from output text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicStopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    StopSequence,
    PauseTurn,
    Compaction,
    ModelContextWindowExceeded,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    Compaction {
        content: String,
        #[serde(default)]
        signature: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Delta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
    CompactionDelta { content: String },
}

struct OpenBlock {
    block: Block,
    item_id: String,
    json: String,
}

pub(super) struct Decoder<'a> {
    tools: &'a [AnthropicTool],
    id: Option<String>,
    blocks: BTreeMap<usize, OpenBlock>,
    call_ids: BTreeSet<String>,
    next_index: usize,
    reason: Option<AnthropicStopReason>,
    usage: Usage,
    raw_usage: Map<String, Value>,
    finished: bool,
}

impl<'a> Decoder<'a> {
    pub(super) fn new(tools: &'a [AnthropicTool]) -> Self {
        Self {
            tools,
            id: None,
            blocks: BTreeMap::new(),
            call_ids: BTreeSet::new(),
            next_index: 0,
            reason: None,
            usage: Usage::default(),
            raw_usage: Map::new(),
            finished: false,
        }
    }

    pub(super) fn finished(&self) -> bool {
        self.finished
    }

    pub(super) fn decode(&mut self, data: &str) -> Result<Vec<ResponseEvent>, ApiError> {
        let frame: Frame = serde_json::from_str(data).map_err(|error| {
            ApiError::NativeStream(format!(
                "invalid Anthropic event at {}:{} ({:?})",
                error.line(),
                error.column(),
                error.classify()
            ))
        })?;
        if self.finished {
            return Err(ApiError::NativeStream(
                "Anthropic event after message_stop".into(),
            ));
        }
        let mut events = Vec::new();
        match frame {
            Frame::Error { error } => {
                return Err(ApiError::NativeStream(format!(
                    "Anthropic {}: {}",
                    error.r#type.chars().take(80).collect::<String>(),
                    error.message.chars().take(1024).collect::<String>()
                )));
            }
            Frame::Ping => {}
            Frame::MessageStart { message } => {
                if self.id.is_some() || message.id.is_empty() || message.id.len() > 512 {
                    return Err(ApiError::NativeStream(
                        "invalid Anthropic message_start".into(),
                    ));
                }
                self.update_usage(&message.usage)?;
                self.id = Some(message.id.clone());
                events.push(ResponseEvent::Created {
                    response_id: Some(message.id),
                });
                if let Some(model) = message.model {
                    events.push(ResponseEvent::ServerModel(model));
                }
            }
            Frame::ContentBlockStart {
                index,
                content_block,
            } => {
                let id = self.id.as_ref().ok_or_else(|| {
                    ApiError::NativeStream("Anthropic block before message_start".into())
                })?;
                if self.reason.is_some() || index != self.next_index || index >= MAX_BLOCKS {
                    return Err(ApiError::NativeStream(
                        "invalid Anthropic block index/order".into(),
                    ));
                }
                let open = OpenBlock {
                    block: content_block,
                    item_id: format!("{id}_{index}"),
                    json: String::new(),
                };
                if let Block::ToolUse { id, name, .. } = &open.block {
                    if id.is_empty()
                        || id.len() > 512
                        || name.is_empty()
                        || name.len() > 512
                        || !self.call_ids.insert(id.clone())
                    {
                        return Err(ApiError::NativeStream(
                            "invalid or repeated Anthropic tool call identity".into(),
                        ));
                    }
                    if !self.tools.iter().any(|tool| tool.name == *name) {
                        tracing::warn!(
                            tool_alias = ?name,
                            call_id = %id,
                            declared_tool_count = self.tools.len(),
                            "Rejecting undeclared Anthropic tool without dispatch"
                        );
                    }
                }
                if !matches!(open.block, Block::Compaction { .. }) {
                    events.push(ResponseEvent::OutputItemAdded(self.item(&open)?));
                }
                match &open.block {
                    Block::Text { text } if !text.is_empty() => {
                        events.push(ResponseEvent::OutputTextDelta(text.clone()))
                    }
                    Block::Thinking { thinking, .. } if !thinking.is_empty() => {
                        events.push(ResponseEvent::ReasoningSummaryDelta {
                            delta: thinking.clone(),
                            summary_index: index as i64,
                        })
                    }
                    _ => {}
                }
                self.blocks.insert(index, open);
                self.next_index += 1;
            }
            Frame::ContentBlockDelta { index, delta } => {
                let open = self.blocks.get_mut(&index).ok_or_else(|| {
                    ApiError::NativeStream("Anthropic delta without open block".into())
                })?;
                match (&mut open.block, delta) {
                    (Block::Text { text }, Delta::TextDelta { text: part }) => {
                        text.push_str(&part);
                        events.push(ResponseEvent::OutputTextDelta(part));
                    }
                    (Block::ToolUse { .. }, Delta::InputJsonDelta { partial_json }) => {
                        // Neither function envelopes nor custom string wrappers
                        // are canonical input until the complete JSON is decoded.
                        open.json.push_str(&partial_json);
                    }
                    (Block::Thinking { thinking, .. }, Delta::ThinkingDelta { thinking: part }) => {
                        thinking.push_str(&part);
                        events.push(ResponseEvent::ReasoningSummaryDelta {
                            delta: part,
                            summary_index: index as i64,
                        });
                    }
                    (
                        Block::Thinking { signature, .. } | Block::Compaction { signature, .. },
                        Delta::SignatureDelta { signature: part },
                    ) => signature.push_str(&part),
                    (
                        Block::Compaction { content, .. },
                        Delta::CompactionDelta { content: part },
                    ) => content.push_str(&part),
                    _ => {
                        return Err(ApiError::NativeStream(
                            "Anthropic delta does not match block".into(),
                        ));
                    }
                }
            }
            Frame::ContentBlockStop { index } => {
                let mut open = self.blocks.remove(&index).ok_or_else(|| {
                    ApiError::NativeStream("Anthropic stop without open block".into())
                })?;
                if let Block::ToolUse { input, .. } = &mut open.block
                    && !open.json.is_empty()
                {
                    *input = serde_json::from_str(&open.json).map_err(|e| {
                        ApiError::NativeStream(format!("invalid Anthropic tool JSON: {e}"))
                    })?;
                }
                if let Block::Thinking { signature, .. } = &open.block
                    && signature.is_empty()
                {
                    return Err(ApiError::NativeStream(
                        "Anthropic thinking signature missing".into(),
                    ));
                }
                if let Block::ToolUse { name, input, .. } = &open.block
                    && self.tools.iter().any(|tool| {
                        tool.name == *name
                            && matches!(tool.identity, AnthropicToolIdentity::Custom { .. })
                    })
                    && (input.as_object().is_none_or(|object| object.len() != 1)
                        || input.get("input").and_then(Value::as_str).is_none())
                {
                    return Err(ApiError::NativeStream(
                        "invalid Anthropic custom tool input wrapper".into(),
                    ));
                }
                let item = self.item(&open)?;
                if let ResponseItem::FunctionCall {
                    call_id, arguments, ..
                } = &item
                {
                    if arguments.is_empty() {
                        return Err(ApiError::NativeStream(
                            "missing Anthropic function arguments envelope".into(),
                        ));
                    }
                    events.push(ResponseEvent::ToolCallInputDelta {
                        item_id: open.item_id.clone(),
                        call_id: Some(call_id.clone()),
                        delta: arguments.clone(),
                    });
                }
                if let ResponseItem::CustomToolCall { call_id, input, .. } = &item {
                    events.push(ResponseEvent::ToolCallInputDelta {
                        item_id: open.item_id,
                        call_id: Some(call_id.clone()),
                        delta: input.clone(),
                    });
                }
                events.push(ResponseEvent::OutputItemDone(item));
            }
            Frame::MessageDelta { delta, usage } => {
                if self.id.is_none() || !self.blocks.is_empty() || self.reason.is_some() {
                    return Err(ApiError::NativeStream(
                        "invalid Anthropic message_delta order".into(),
                    ));
                }
                self.update_usage(&usage)?;
                self.reason = delta.stop_reason;
            }
            Frame::MessageStop => {
                let response_id = self.id.clone().ok_or_else(|| {
                    ApiError::NativeStream("Anthropic message_stop before message_start".into())
                })?;
                let reason = self.reason.ok_or_else(|| {
                    ApiError::NativeStream("Anthropic stop reason missing".into())
                })?;
                if !self.blocks.is_empty() {
                    return Err(ApiError::NativeStream(
                        "Anthropic message_stop with unfinished blocks".into(),
                    ));
                }
                let token_usage = self.usage.finish()?;
                self.finished = true;
                events.push(ResponseEvent::Completed {
                    response_id,
                    token_usage: Some(token_usage),
                    usage_metadata: Some(ResponseUsageMetadata {
                        amount: None,
                        reported_reasoning_tokens: self.usage.reported_reasoning_tokens(),
                        metadata: Some(json!({
                            "provider": "anthropic",
                            "stop_reason": reason,
                            "usage": self.raw_usage,
                        })),
                    }),
                    end_turn: Some(matches!(
                        reason,
                        AnthropicStopReason::EndTurn
                            | AnthropicStopReason::Refusal
                            | AnthropicStopReason::StopSequence
                    )),
                });
            }
        }
        Ok(events)
    }

    fn update_usage(&mut self, raw: &Value) -> Result<(), ApiError> {
        self.usage.update(raw)?;
        let object = raw
            .as_object()
            .ok_or_else(|| ApiError::NativeStream("Anthropic usage is not an object".into()))?;
        self.raw_usage.extend(object.clone());
        Ok(())
    }

    fn item(&self, open: &OpenBlock) -> Result<ResponseItem, ApiError> {
        let id = Some(ResponseItemId::from_server(open.item_id.clone()));
        Ok(match &open.block {
            Block::Text { text } => ResponseItem::Message {
                id,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText { text: text.clone() }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
            Block::ToolUse {
                id: call_id,
                name: alias,
                input,
            } => {
                if !input.is_object() || call_id.is_empty() {
                    return Err(ApiError::NativeStream(
                        "invalid Anthropic tool block".into(),
                    ));
                }
                let Some(tool) = self.tools.iter().find(|tool| tool.name == *alias) else {
                    // Preserve the received name and body, but not as an executable
                    // function: the local registry may contain unadvertised tools.
                    return Ok(ResponseItem::RejectedToolCall {
                        id,
                        call: NativeToolCall::Anthropic {
                            call_id: call_id.clone(),
                            name: alias.clone(),
                            input: input.clone(),
                        },
                    });
                };
                match &tool.identity {
                    AnthropicToolIdentity::Function { name, namespace } => {
                        ResponseItem::FunctionCall {
                            id,
                            name: name.clone(),
                            namespace: namespace.clone(),
                            arguments: function_arguments(input, &open.json)?,
                            encrypted_function_args: None,
                            call_id: call_id.clone(),
                            internal_chat_message_metadata_passthrough: None,
                        }
                    }
                    AnthropicToolIdentity::Custom { name, namespace } => {
                        let input = if input.as_object().is_some_and(Map::is_empty)
                            && open.json.is_empty()
                        {
                            String::new()
                        } else {
                            input
                                .get("input")
                                .and_then(Value::as_str)
                                .ok_or_else(|| {
                                    ApiError::NativeStream(
                                        "Anthropic custom tool input missing".into(),
                                    )
                                })?
                                .to_owned()
                        };
                        ResponseItem::CustomToolCall {
                            id,
                            status: None,
                            name: name.clone(),
                            namespace: namespace.clone(),
                            input,
                            call_id: call_id.clone(),
                            internal_chat_message_metadata_passthrough: None,
                        }
                    }
                }
            }
            Block::Thinking {
                thinking,
                signature,
            } => ResponseItem::Reasoning {
                id,
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: thinking.clone(),
                }],
                content: None,
                encrypted_content: Some(signature.clone()),
                internal_chat_message_metadata_passthrough: None,
            },
            Block::RedactedThinking { data } => ResponseItem::Reasoning {
                id,
                summary: Vec::new(),
                content: None,
                encrypted_content: Some(format!("claude-redacted-thinking:{data}")),
                internal_chat_message_metadata_passthrough: None,
            },
            Block::Compaction { content, signature } => {
                // Partial compaction blocks cannot inhabit the validated canonical type.
                let compaction: AnthropicSignedCompaction =
                    serde_json::from_value(json!({"content":content,"signature":signature}))
                        .map_err(|e| {
                            ApiError::NativeStream(format!(
                                "invalid signed Anthropic compaction: {e}"
                            ))
                        })?;
                ResponseItem::NativeCompaction {
                    id,
                    compaction: NativeCompaction::Anthropic(compaction),
                }
            }
        })
    }
}

#[cfg(test)]
#[path = "decoder_tests.rs"]
mod tests;
