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
use codex_protocol::models::ToolCallRejection;
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
/// Describes a native refusal after upstream usage is recorded, with the policy area and
/// explanation Anthropic reports for it.
pub fn anthropic_refusal(metadata: &ResponseUsageMetadata) -> Option<String> {
    const REFUSED: &str = "The model refused the request";
    if anthropic_stop_reason(metadata)? != AnthropicStopReason::Refusal {
        return None;
    }
    let details = metadata.metadata.as_ref()?.get("stop_details");
    let field = |name| {
        details
            .and_then(|details| details.get(name))
            .and_then(Value::as_str)
            .map(|value| value.chars().take(1024).collect::<String>())
    };
    Some(match (field("category"), field("explanation")) {
        (Some(category), Some(explanation)) => format!("{REFUSED} ({category}): {explanation}"),
        (Some(category), None) => format!("{REFUSED} ({category})"),
        (None, Some(explanation)) => format!("{REFUSED}: {explanation}"),
        (None, None) => REFUSED.to_string(),
    })
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
    MessageStart {
        message: Message,
    },
    ContentBlockStart {
        index: usize,
        content_block: Block,
    },
    ContentBlockDelta {
        index: usize,
        delta: Delta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: MessageDelta,
        usage: Value,
    },
    MessageStop,
    Ping,
    Error {
        error: NativeError,
    },
    /// Anthropic may add event types; an unknown one carries nothing this decoder acts on.
    #[serde(other)]
    Unknown,
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
    /// A classifier decline's policy `category` and `explanation`.
    stop_details: Option<Value>,
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
#[serde(tag = "type")]
enum Delta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
    #[serde(rename = "compaction_delta")]
    Compaction { content: String },
}

struct OpenBlock {
    block: Block,
    item_id: ResponseItemId,
    json: String,
}

pub(super) struct Decoder<'a> {
    tools: &'a [AnthropicTool],
    id: Option<String>,
    blocks: BTreeMap<usize, OpenBlock>,
    call_ids: BTreeSet<String>,
    next_index: usize,
    reason: Option<AnthropicStopReason>,
    /// Reported with a refusal's stop reason.
    stop_details: Option<Value>,
    usage: Usage,
    raw_usage: Map<String, Value>,
    finished: bool,
    /// Completed text and thinking, held back until a call or the stop commits them.
    withheld: Vec<ResponseItem>,
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
            stop_details: None,
            usage: Usage::default(),
            raw_usage: Map::new(),
            finished: false,
            withheld: Vec::new(),
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
                // Errors that invite a retry stay typed wherever they arrive: a retry rebuilds
                // from recorded history, which ends with an answered call or before this reply.
                let message = error.message.chars().take(1024).collect::<String>();
                match error.r#type.as_str() {
                    "overloaded_error" => {
                        return Err(ApiError::ServerOverloaded { retry_after: None });
                    }
                    "rate_limit_error" => {
                        return Err(ApiError::RateLimitExceeded {
                            message,
                            retry_after: None,
                        });
                    }
                    "api_error" => {
                        return Err(ApiError::Retryable {
                            message: format!("Anthropic api_error: {message}"),
                            retry_after: None,
                        });
                    }
                    _ => {}
                }
                return Err(ApiError::NativeStream(format!(
                    "Anthropic {}: {}",
                    error.r#type.chars().take(80).collect::<String>(),
                    message
                )));
            }
            Frame::Ping | Frame::Unknown => {}
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
                    item_id: ResponseItemId::from_server(format!("{id}_{index}")),
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
                    let mut item = self.item(&open)?;
                    // Initial text is delivered by the addressed delta below, not twice.
                    match &mut item {
                        ResponseItem::Message { content, .. } => content.clear(),
                        ResponseItem::Reasoning { summary, .. } => summary.clear(),
                        _ => {}
                    }
                    events.push(ResponseEvent::OutputItemAdded(item));
                }
                match &open.block {
                    Block::Text { text } if !text.is_empty() => {
                        events.push(ResponseEvent::OutputTextDelta {
                            item_id: open.item_id.clone(),
                            delta: text.clone(),
                        });
                    }
                    Block::Thinking { thinking, .. } if !thinking.is_empty() => {
                        events.push(ResponseEvent::ReasoningSummaryDelta {
                            item_id: open.item_id.clone(),
                            delta: thinking.clone(),
                            summary_index: 0,
                        });
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
                    (Block::Text { text }, Delta::Text { text: part }) => {
                        text.push_str(&part);
                        events.push(ResponseEvent::OutputTextDelta {
                            item_id: open.item_id.clone(),
                            delta: part,
                        });
                    }
                    (Block::ToolUse { .. }, Delta::InputJson { partial_json }) => {
                        // Neither function envelopes nor custom string wrappers
                        // are canonical input until the complete JSON is decoded.
                        open.json.push_str(&partial_json);
                    }
                    (Block::Thinking { thinking, .. }, Delta::Thinking { thinking: part }) => {
                        thinking.push_str(&part);
                        events.push(ResponseEvent::ReasoningSummaryDelta {
                            item_id: open.item_id.clone(),
                            delta: part,
                            summary_index: 0,
                        });
                    }
                    (
                        Block::Thinking { signature, .. } | Block::Compaction { signature, .. },
                        Delta::Signature { signature: part },
                    ) => signature.push_str(&part),
                    (Block::Compaction { content, .. }, Delta::Compaction { content: part }) => {
                        content.push_str(&part);
                    }
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
                if let Block::ToolUse { name, input, .. } = &mut open.block {
                    let declared = self.tools.iter().any(|tool| tool.name == *name);
                    if !open.json.is_empty() {
                        *input = match serde_json::from_str(&open.json) {
                            Ok(input) => input,
                            Err(_) if declared => json!({}),
                            Err(error) => {
                                return Err(ApiError::NativeStream(format!(
                                    "invalid Anthropic tool JSON: {error}"
                                )));
                            }
                        };
                    }
                }
                if let Block::Thinking { signature, .. } = &open.block
                    && signature.is_empty()
                {
                    return Err(ApiError::NativeStream(
                        "Anthropic thinking signature missing".into(),
                    ));
                }
                let item = match &open.block {
                    Block::ToolUse { id, name, input }
                        if input.as_object().is_some_and(Map::is_empty)
                            && self.tools.iter().any(|tool| tool.name == *name) =>
                    {
                        ResponseItem::RejectedToolCall {
                            id: Some(open.item_id.clone()),
                            reason: ToolCallRejection::MalformedInput,
                            call: NativeToolCall::Anthropic {
                                call_id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            },
                        }
                    }
                    _ => self.item(&open)?,
                };
                // Core records a completed item at once. Text and thinking wait for the call
                // that follows them, or for the stop, so an interruption before either leaves
                // nothing recorded and the attempt replays whole.
                if !matches!(open.block, Block::ToolUse { .. }) {
                    self.withheld.push(item);
                    return Ok(events);
                }
                events.extend(self.withheld.drain(..).map(ResponseEvent::OutputItemDone));
                if let ResponseItem::FunctionCall {
                    call_id, arguments, ..
                } = &item
                {
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
                // Anthropic returns a classifier decline that fires inside a block, such as a
                // tool-use block it will not retry on a fallback model, without closing it. The
                // unfinished block (partial input or text, unsigned thinking) is never committed.
                let declined = delta.stop_reason == Some(AnthropicStopReason::Refusal);
                if self.id.is_none() || (!declined && !self.blocks.is_empty()) {
                    return Err(ApiError::NativeStream(format!(
                        "invalid Anthropic message_delta order (stop reason {:?}, {} open blocks)",
                        delta.stop_reason,
                        self.blocks.len()
                    )));
                }
                self.blocks.clear();
                // One or more message_delta events may arrive: usage is cumulative, and a later
                // stop reason supersedes an earlier one.
                self.update_usage(&usage)?;
                if delta.stop_reason.is_some() {
                    self.reason = delta.stop_reason;
                    self.stop_details = delta.stop_details;
                }
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
                // A refusal's output is incomplete and Anthropic directs that it be discarded.
                if reason != AnthropicStopReason::Refusal {
                    events.extend(self.withheld.drain(..).map(ResponseEvent::OutputItemDone));
                }
                let mut metadata = json!({
                    "provider": "anthropic",
                    "stop_reason": reason,
                    "usage": self.raw_usage,
                });
                if let Some(details) = self.stop_details.take() {
                    metadata["stop_details"] = details;
                }
                events.push(ResponseEvent::Completed {
                    reasoning_removals: None,
                    response_id,
                    token_usage: Some(token_usage),
                    usage_metadata: Some(ResponseUsageMetadata {
                        amount: None,
                        reported_reasoning_tokens: self.usage.reported_reasoning_tokens(),
                        metadata: Some(metadata),
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
        let id = Some(open.item_id.clone());
        Ok(match &open.block {
            Block::Text { text } => ResponseItem::Message {
                id,
                encrypted_content: None,
                status: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: text.clone(),
                    annotations: None,
                    logprobs: None,
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
            Block::ToolUse {
                id: call_id,
                name: alias,
                input,
            } => {
                let rejection = |reason| ResponseItem::RejectedToolCall {
                    id: id.clone(),
                    reason,
                    call: NativeToolCall::Anthropic {
                        call_id: call_id.clone(),
                        name: alias.clone(),
                        input: if input.is_object() {
                            input.clone()
                        } else {
                            json!({})
                        },
                    },
                };
                if !input.is_object() || call_id.is_empty() {
                    if !call_id.is_empty() && self.tools.iter().any(|tool| tool.name == *alias) {
                        return Ok(rejection(ToolCallRejection::MalformedInput));
                    }
                    return Err(ApiError::NativeStream(
                        "invalid Anthropic tool block".into(),
                    ));
                }
                let Some(tool) = self.tools.iter().find(|tool| tool.name == *alias) else {
                    // Preserve the received name and body, but not as an executable
                    // function: the local registry may contain unadvertised tools.
                    return Ok(rejection(ToolCallRejection::Undeclared));
                };
                match &tool.identity {
                    AnthropicToolIdentity::Function { name, namespace } => {
                        let Ok(arguments) = function_arguments(input, &open.json) else {
                            return Ok(rejection(ToolCallRejection::MalformedInput));
                        };
                        ResponseItem::FunctionCall {
                            id,
                            encrypted_content: None,
                            status: None,
                            name: name.clone(),
                            namespace: namespace.clone(),
                            arguments,
                            encrypted_function_args: Some(Vec::new()),
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
                            let Some(input) = input
                                .as_object()
                                .filter(|object| object.len() == 1)
                                .and_then(|object| object.get("input"))
                                .and_then(Value::as_str)
                            else {
                                return Ok(rejection(ToolCallRejection::MalformedInput));
                            };
                            input.to_owned()
                        };
                        ResponseItem::CustomToolCall {
                            id,
                            encrypted_content: None,
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
