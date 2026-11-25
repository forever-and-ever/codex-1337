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
use std::collections::HashSet;

const USER: &str = "user";
const ASSISTANT: &str = "assistant";
const NOTIFICATION_CLOSE: &str = "</tool_notification>";

#[derive(Serialize)]
pub(super) struct Message {
    pub role: &'static str,
    pub content: Vec<Value>,
}

pub(super) struct History {
    pub system: Vec<Value>,
    pub messages: Vec<Message>,
}

#[derive(Clone, Copy)]
pub(super) enum CallKind {
    Function,
    Custom,
    Rejected,
}

#[derive(Clone, Copy)]
enum ResultKind {
    Function,
    Custom,
}

/// One canonical item encoded, with its role in tool pairing.
enum Entry {
    /// Leading instructions, hoisted into the system prompt.
    System(Vec<Value>),
    Free {
        role: &'static str,
        content: Vec<Value>,
    },
    Call {
        id: String,
        kind: CallKind,
        tool_use: Value,
    },
    Result {
        id: String,
        kind: ResultKind,
        result: Value,
    },
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
    let mut encoder = Encoder::new(instructions, tools, modalities);
    for item in input {
        encoder.push(item)?;
    }
    encoder.finish()
}

/// Incremental encoding: `push` either accepts an item or leaves the encoder unchanged.
///
/// A result belongs to the immediately preceding assistant tool batch. The encoder never
/// reorders or invents results: that would change both semantics and cache keys.
pub(super) struct Encoder<'a> {
    tools: &'a [AnthropicTool],
    modalities: &'a [InputModality],
    history: History,
    calls: HashSet<String>,
    /// The open batch's calls that await results, in call order.
    pending: Vec<(String, CallKind)>,
    /// Content recorded while the open batch awaited results. Canonical history is
    /// chronological, and hosts record goal updates and delegation receipts the moment they
    /// are accepted, even during a long tool call. A `tool_result` must directly answer its
    /// `tool_use`, so this content follows the completed batch, where the model could first
    /// have observed it.
    deferred: Vec<Message>,
}

impl<'a> Encoder<'a> {
    pub(super) fn new(
        instructions: &str,
        tools: &'a [AnthropicTool],
        modalities: &'a [InputModality],
    ) -> Self {
        let mut system = Vec::new();
        if !instructions.is_empty() {
            system.push(json!({"type":"text", "text":instructions}));
        }
        Self {
            tools,
            modalities,
            history: History {
                system,
                messages: Vec::new(),
            },
            calls: HashSet::new(),
            pending: Vec::new(),
            deferred: Vec::new(),
        }
    }

    fn last_role(&self) -> Option<&'static str> {
        self.history.messages.last().map(|message| message.role)
    }

    /// Nothing but a signed checkpoint precedes the next item, so instructions arriving now
    /// open the window and belong to `system`, as they do in a first window.
    // TELOS(reminders-not-system): later instructions keep their position as reminders.
    fn opens_window(&self) -> bool {
        self.history.messages.iter().all(|message| {
            message.role == ASSISTANT
                && message
                    .content
                    .iter()
                    .all(|block| block["type"] == "compaction")
        })
    }

    /// The open batch's calls that await results, in call order.
    pub(super) fn pending(&self) -> &[(String, CallKind)] {
        &self.pending
    }

    /// Some results of the open batch are encoded and others are outstanding.
    pub(super) fn awaits_partial_results(&self) -> bool {
        !self.pending.is_empty() && self.last_role() == Some(USER)
    }

    pub(super) fn push(&mut self, item: &ResponseItem) -> Result<(), ApiError> {
        match self.entry(item)? {
            Entry::System(blocks) => self.history.system.extend(blocks),
            Entry::Free { content, .. } if content.is_empty() => {
                return Err(invalid("empty Anthropic message"));
            }
            Entry::Free { role, content } => {
                if self.pending.is_empty()
                    || (role == ASSISTANT && self.last_role() == Some(ASSISTANT))
                {
                    self.append(role, content);
                } else {
                    self.deferred.push(Message { role, content });
                }
            }
            Entry::Call { id, kind, tool_use } => {
                if self.calls.contains(&id) {
                    return Err(invalid("duplicate Anthropic tool call ID"));
                }
                if self.awaits_partial_results() {
                    return Err(invalid(
                        "Anthropic assistant interrupts incomplete tool result batch",
                    ));
                }
                self.calls.insert(id.clone());
                self.pending.push((id, kind));
                self.append(ASSISTANT, vec![tool_use]);
            }
            Entry::Result {
                id,
                kind,
                mut result,
            } => {
                let Some(index) = self.pending.iter().position(|(call, _)| *call == id) else {
                    return Err(invalid(match kind {
                        ResultKind::Function => "unpaired Anthropic tool result",
                        ResultKind::Custom => "unpaired Anthropic custom tool result",
                    }));
                };
                match (kind, self.pending[index].1) {
                    (ResultKind::Function, CallKind::Custom) => {
                        return Err(invalid("function result paired with custom Anthropic tool"));
                    }
                    (ResultKind::Custom, CallKind::Function | CallKind::Rejected) => {
                        return Err(invalid("custom result paired with function Anthropic tool"));
                    }
                    // FunctionCallOutputPayload's success hint is not serialized.
                    // The durable rejected call remains authoritative on resume.
                    (ResultKind::Function, CallKind::Rejected) => result["is_error"] = json!(true),
                    (ResultKind::Function, CallKind::Function)
                    | (ResultKind::Custom, CallKind::Custom) => {}
                }
                self.pending.remove(index);
                self.append(USER, vec![result]);
                if self.pending.is_empty() {
                    for Message { role, content } in std::mem::take(&mut self.deferred) {
                        self.append(role, content);
                    }
                }
            }
        }
        Ok(())
    }

    fn append(&mut self, role: &'static str, content: Vec<Value>) {
        match self.history.messages.last_mut() {
            Some(message) if message.role == role => message.content.extend(content),
            Some(_) | None => self.history.messages.push(Message { role, content }),
        }
    }

    pub(super) fn finish(mut self) -> Result<History, ApiError> {
        if !self.pending.is_empty() {
            return Err(invalid("Anthropic history ends with unresolved tool calls"));
        }
        if self.history.messages.is_empty() {
            return Err(invalid(
                "Anthropic history contains no conversation messages",
            ));
        }
        for message in &mut self.history.messages {
            if message.role == USER {
                separate(&mut message.content);
            }
        }
        Ok(self.history)
    }

    fn entry(&self, item: &ResponseItem) -> Result<Entry, ApiError> {
        Ok(match item {
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
                Entry::Free {
                    role: USER,
                    content: vec![
                        json!({"type":"text", "text":agent_message(author, recipient, &text)}),
                    ],
                }
            }
            ResponseItem::Message { role, content, .. } => {
                let blocks = content
                    .iter()
                    .map(|part| match part {
                        ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                            Ok(json!({"type":"text", "text":text}))
                        }
                        ContentItem::InputImage { image, .. } => {
                            encode_image(image, self.modalities)
                        }
                        ContentItem::InputAudio { .. } => {
                            Err(invalid("Anthropic audio input is unsupported"))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                match role.as_str() {
                    "system" | "developer" => {
                        if blocks.iter().any(|block| block["type"] != "text") {
                            return Err(invalid(
                                "Anthropic system instructions must contain only text",
                            ));
                        }
                        if self.opens_window() {
                            return Ok(Entry::System(blocks));
                        }
                        // Native system-message placement cannot represent arbitrary
                        // canonical updates. Keep every update at its original position
                        // across requests, without reordering a signed or cached prefix.
                        let reminders = blocks.into_iter().map(|block| {
                            let text = block["text"].as_str().ok_or_else(|| invalid("Anthropic instruction block must contain text"))?;
                            Ok(json!({"type":"text", "text":format!("<system-reminder>\n{text}\n</system-reminder>")}))
                        }).collect::<Result<Vec<_>, ApiError>>()?;
                        Entry::Free {
                            role: USER,
                            content: reminders,
                        }
                    }
                    "user" => Entry::Free {
                        role: USER,
                        content: blocks,
                    },
                    "assistant" => Entry::Free {
                        role: ASSISTANT,
                        content: blocks,
                    },
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
                if encrypted_function_args
                    .as_ref()
                    .is_some_and(|fields| !fields.is_empty())
                {
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
                let alias = history_alias(self.tools, &identity)?;
                Entry::Call {
                    id: call_id.clone(),
                    kind: CallKind::Function,
                    tool_use: json!({"type":"tool_use", "id":call_id, "name":alias, "input":{"arguments":input}}),
                }
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
                let alias = history_alias(self.tools, &identity)?;
                Entry::Call {
                    id: call_id.clone(),
                    kind: CallKind::Custom,
                    tool_use: json!({"type":"tool_use", "id":call_id, "name":alias, "input":{"input":input}}),
                }
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
                Entry::Call {
                    id: call_id.clone(),
                    kind: CallKind::Rejected,
                    tool_use: json!({"type":"tool_use", "id":call_id, "name":name, "input":input}),
                }
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
                let failed = if output.success == Some(false) {
                    " failed=\"true\""
                } else {
                    ""
                };
                let open = format!("<tool_notification from={}{failed}>", json!(source));
                let blocks = encode_result_blocks(output, self.modalities)?;
                // Text alone shares one block with its envelope, at no cost in paragraph breaks.
                let texts = blocks
                    .iter()
                    .map(|block| block["text"].as_str().filter(|_| block["type"] == "text"))
                    .collect::<Option<Vec<_>>>();
                let content = match texts {
                    Some(texts) => vec![json!({"type":"text", "text":format!(
                        "{open}\n{}\n{NOTIFICATION_CLOSE}",
                        texts.join("\n")
                    )})],
                    None => std::iter::once(json!({"type":"text", "text":open}))
                        .chain(blocks)
                        .chain([json!({"type":"text", "text":NOTIFICATION_CLOSE})])
                        .collect(),
                };
                Entry::Free {
                    role: USER,
                    content,
                }
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                let call_id = call_id
                    .as_deref()
                    .ok_or_else(|| invalid("Anthropic tool result has no call ID"))?;
                Entry::Result {
                    id: call_id.to_owned(),
                    kind: ResultKind::Function,
                    result: encode_result(call_id, output, self.modalities)?,
                }
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => Entry::Result {
                id: call_id.clone(),
                kind: ResultKind::Custom,
                result: encode_result(call_id, output, self.modalities)?,
            },
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
                Entry::Free {
                    role: ASSISTANT,
                    content: vec![block],
                }
            }
            ResponseItem::NativeCompaction {
                compaction: NativeCompaction::Anthropic(compaction),
                ..
            } => Entry::Free {
                role: ASSISTANT,
                content: vec![
                    json!({"type":"compaction", "content":compaction.content(), "signature":compaction.signature()}),
                ],
            },
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
        })
    }
}

/// The provider joins adjacent text blocks without a separator. A text block that follows
/// another therefore opens its own paragraph: prefixing the later block leaves every block
/// already sent, and so the cached prefix, unchanged.
pub(super) fn separate(blocks: &mut [Value]) {
    let mut follows_text = false;
    for block in blocks {
        let text = block["type"] == "text";
        if text
            && follows_text
            && let Some(body) = block["text"].as_str()
        {
            block["text"] = Value::String(format!("\n\n{body}"));
        }
        follows_text = text;
    }
}

/// Canonical mail opens with upstream's header, which restates the addresses the item
/// already holds. Native Claude reads one envelope; text without that header is kept whole.
fn agent_message(author: &str, recipient: &str, text: &str) -> String {
    let addresses = format!("from={} to={}", json!(author), json!(recipient));
    let typed = [
        ("MESSAGE", "message"),
        ("NEW_TASK", "new_task"),
        ("FINAL_ANSWER", "final_answer"),
    ]
    .into_iter()
    .find_map(|(wire, kind)| {
        let header =
            format!("Message Type: {wire}\nTask name: {recipient}\nSender: {author}\nPayload:\n");
        text.strip_prefix(&header).map(|payload| (kind, payload))
    });
    match typed {
        Some((kind, payload)) => {
            format!("<agent_message {addresses} type=\"{kind}\">\n{payload}\n</agent_message>")
        }
        None => format!("<agent_message {addresses}>\n{text}\n</agent_message>"),
    }
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
