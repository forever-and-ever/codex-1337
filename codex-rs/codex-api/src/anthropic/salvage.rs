//! Narrows unencodable canonical history to what the native encoder accepts.

use super::AnthropicRequest;
use super::history::CallKind;
use super::history::Encoder;
use crate::ApiError;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::InputModality;

/// Canonical history narrowed to what the native encoder accepts.
#[derive(Debug)]
pub struct AnthropicSalvage {
    pub items: Vec<ResponseItem>,
    /// Every concession, in history order; shedding, which weighs the whole history, comes last.
    pub repairs: Vec<AnthropicRepair>,
}

/// One concession that keeps an otherwise unencodable history servable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnthropicRepair {
    /// An item the encoder rejects, for the stated reason, was omitted.
    Omitted(String),
    /// Media or opaque parts the encoder rejects were replaced by text placeholders.
    Degraded(String),
    /// This many calls left without results were answered with synthetic errors.
    Closed(usize),
    /// The media of this many oldest items was replaced by text placeholders so that the
    /// request fits Anthropic's size limit.
    Shed(usize),
}

/// Narrows canonical history, item by item, to what the native encoder accepts.
///
/// The result encodes unless it holds no conversation at all, and its messages leave a request
/// room for instructions and tool declarations, shedding the oldest media first. Accepted items
/// keep their exact encoding, so the cacheable prefix before the first repair is undisturbed.
/// Tool declarations only choose replay aliases and never decide acceptance, so none are
/// consulted.
pub fn salvage_anthropic_history(
    input: Vec<ResponseItem>,
    modalities: &[InputModality],
) -> AnthropicSalvage {
    salvage_within(input, modalities, MESSAGE_CEILING)
}

/// Encoded messages may fill a request but for room for instructions, tool declarations and
/// parameters, which never approach 2 MB.
const MESSAGE_CEILING: usize = AnthropicRequest::MAX_BODY_BYTES - 2_000_000;

/// [`salvage_anthropic_history`] under an explicit ceiling on encoded message bytes.
pub(super) fn salvage_within(
    input: Vec<ResponseItem>,
    modalities: &[InputModality],
    ceiling: usize,
) -> AnthropicSalvage {
    let mut encoder = Encoder::new("", &[], modalities);
    let mut salvage = AnthropicSalvage {
        items: Vec::with_capacity(input.len()),
        repairs: Vec::new(),
    };
    for item in input {
        if matches!(
            item,
            ResponseItem::FunctionCall { .. }
                | ResponseItem::CustomToolCall { .. }
                | ResponseItem::RejectedToolCall { .. }
        ) && encoder.awaits_partial_results()
        {
            // A call cannot follow partial results: answer the open batch first.
            salvage.close(&mut encoder);
        }
        let reason = match encoder.push(&item) {
            Ok(()) => {
                salvage.items.push(item);
                continue;
            }
            Err(ApiError::InvalidPrompt { message } | ApiError::InvalidRequest { message }) => {
                message
            }
            Err(error) => error.to_string(),
        };
        if let Some(degraded) = degrade(&item)
            && encoder.push(&degraded).is_ok()
        {
            salvage.items.push(degraded);
            salvage.repairs.push(AnthropicRepair::Degraded(reason));
        } else {
            salvage.repairs.push(AnthropicRepair::Omitted(reason));
        }
    }
    if !encoder.pending().is_empty() {
        salvage.close(&mut encoder);
    }
    if let Ok(history) = encoder.finish()
        && let Ok(encoded) = serde_json::to_vec(&history.messages)
    {
        salvage.shed(encoded.len().saturating_sub(ceiling));
    }
    salvage
}

impl AnthropicSalvage {
    /// Answers every call of the open batch with a synthetic error result.
    fn close(&mut self, encoder: &mut Encoder<'_>) {
        let pending = encoder.pending().to_vec();
        for (call_id, kind) in &pending {
            let output = FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("No result was recorded for this call.".into()),
                success: Some(false),
            };
            let item = match kind {
                CallKind::Custom => ResponseItem::CustomToolCallOutput {
                    id: None,
                    call_id: call_id.clone(),
                    name: None,
                    output,
                    internal_chat_message_metadata_passthrough: None,
                },
                CallKind::Function | CallKind::Rejected => ResponseItem::FunctionCallOutput {
                    id: None,
                    call_id: Some(call_id.clone()),
                    name: None,
                    namespace: None,
                    output,
                    internal_chat_message_metadata_passthrough: None,
                },
            };
            let Ok(()) = encoder.push(&item) else {
                unreachable!("a synthetic result answers its pending call");
            };
            self.items.push(item);
        }
        self.repairs.push(AnthropicRepair::Closed(pending.len()));
    }

    /// Replaces the media of the oldest items until `excess` encoded bytes are gone.
    fn shed(&mut self, mut excess: usize) {
        let mut shed = 0;
        for item in &mut self.items {
            if excess == 0 {
                break;
            }
            let bytes = image_bytes(item);
            if bytes > 0
                && let Some(degraded) = degrade(item)
            {
                *item = degraded;
                excess = excess.saturating_sub(bytes);
                shed += 1;
            }
        }
        if shed > 0 {
            self.repairs.push(AnthropicRepair::Shed(shed));
        }
    }
}

/// Replaces every media or opaque part of a message or tool result with a text placeholder.
fn degrade(item: &ResponseItem) -> Option<ResponseItem> {
    let mut item = item.clone();
    let mut degraded = false;
    match &mut item {
        ResponseItem::Message { content, .. } => {
            for part in content {
                let placeholder = match part {
                    ContentItem::InputText { .. } | ContentItem::OutputText { .. } => continue,
                    ContentItem::InputImage { .. } => "[image omitted]",
                    ContentItem::InputAudio { .. } => "[audio omitted]",
                };
                *part = ContentItem::InputText {
                    text: placeholder.to_owned(),
                };
                degraded = true;
            }
        }
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => {
            for part in output.content_items_mut().into_iter().flatten() {
                let placeholder = match part {
                    FunctionCallOutputContentItem::InputText { .. } => continue,
                    FunctionCallOutputContentItem::InputImage { .. } => "[image omitted]",
                    FunctionCallOutputContentItem::InputAudio { .. } => "[audio omitted]",
                    FunctionCallOutputContentItem::EncryptedContent { .. } => {
                        "[encrypted content omitted]"
                    }
                };
                *part = FunctionCallOutputContentItem::InputText {
                    text: placeholder.to_owned(),
                };
                degraded = true;
            }
        }
        _ => {}
    }
    degraded.then_some(item)
}

/// Bytes of the image references in a message or tool result. Each is shorter than the encoded
/// block its placeholder replaces, so shedding the item saves at least this many bytes.
fn image_bytes(item: &ResponseItem) -> usize {
    let bytes = |image: &ImageReference| match image {
        ImageReference::Inline { image_url } => image_url.len(),
        ImageReference::File { .. } => 0,
    };
    match item {
        ResponseItem::Message { content, .. } => content
            .iter()
            .map(|part| match part {
                ContentItem::InputImage { image, .. } => bytes(image),
                ContentItem::InputText { .. }
                | ContentItem::OutputText { .. }
                | ContentItem::InputAudio { .. } => 0,
            })
            .sum(),
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => output
            .content_items()
            .into_iter()
            .flatten()
            .map(|part| match part {
                FunctionCallOutputContentItem::InputImage { image, .. } => bytes(image),
                FunctionCallOutputContentItem::InputText { .. }
                | FunctionCallOutputContentItem::InputAudio { .. }
                | FunctionCallOutputContentItem::EncryptedContent { .. } => 0,
            })
            .sum(),
        _ => 0,
    }
}
