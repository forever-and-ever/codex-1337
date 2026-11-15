use super::ResponsesEventError;
use super::ResponsesStreamEvent;
use crate::ResponseEvent;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;
use std::mem::Discriminant;
use std::mem::discriminant;

#[derive(PartialEq, Eq)]
enum ItemKind {
    Message(String),
    Reasoning,
    CustomTool(String),
    FunctionTool(String),
    Other(Discriminant<ResponseItem>),
}

impl ItemKind {
    fn of(item: &ResponseItem) -> Self {
        match item {
            ResponseItem::Message { role, .. } => Self::Message(role.clone()),
            ResponseItem::Reasoning { .. } => Self::Reasoning,
            ResponseItem::CustomToolCall { call_id, .. } => Self::CustomTool(call_id.clone()),
            ResponseItem::FunctionCall { call_id, .. } => Self::FunctionTool(call_id.clone()),
            _ => Self::Other(discriminant(item)),
        }
    }
}

#[derive(Default)]
pub(crate) struct ResponsesItemIdentity {
    open: BTreeMap<ResponseItemId, ItemKind>,
}

impl ResponsesItemIdentity {
    pub(super) fn handles(event: &ResponsesStreamEvent) -> bool {
        matches!(
            event.kind.as_str(),
            "response.output_item.added"
                | "response.output_item.done"
                | "response.output_text.delta"
                | "response.custom_tool_call_input.delta"
                | "response.reasoning_summary_text.delta"
                | "response.reasoning_summary_text.done"
                | "response.reasoning_text.delta"
                | "response.reasoning_summary_part.added"
        )
    }

    fn invalid(message: &str) -> ResponsesEventError {
        ResponsesEventError::ItemIdentity(format!("invalid response item identity: {message}"))
    }

    fn resolve(
        &self,
        explicit: Option<String>,
        compatible: impl Fn(&ItemKind) -> bool,
    ) -> Result<ResponseItemId, ResponsesEventError> {
        if let Some(id) = explicit.filter(|id| !id.is_empty()) {
            let id = ResponseItemId::from_server(id);
            return match self.open.get(&id) {
                Some(kind) if compatible(kind) => Ok(id),
                Some(_) => Err(Self::invalid("item category mismatch")),
                None => Err(Self::invalid("delta without matching open item")),
            };
        }
        let mut matches = self.open.iter().filter(|(_, kind)| compatible(kind));
        let Some((id, _)) = matches.next() else {
            return Err(Self::invalid("missing ID without compatible open item"));
        };
        if matches.next().is_some() {
            return Err(Self::invalid(
                "missing ID with multiple compatible open items",
            ));
        }
        Ok(id.clone())
    }

    pub(super) fn process(
        &mut self,
        event: ResponsesStreamEvent,
    ) -> Result<Option<ResponseEvent>, ResponsesEventError> {
        let output = match event.kind.as_str() {
            "response.output_item.added" | "response.output_item.done" => {
                let Some(value) = event.item else {
                    return Ok(None);
                };
                let Ok(mut item) = serde_json::from_value::<ResponseItem>(value) else {
                    return Err(Self::invalid("malformed output item"));
                };
                let Some(prefix) = item.id_prefix() else {
                    return Ok(Some(if event.kind == "response.output_item.added" {
                        ResponseEvent::OutputItemAdded(item)
                    } else {
                        ResponseEvent::OutputItemDone(item)
                    }));
                };
                let kind = ItemKind::of(&item);
                if let (Some(inner), Some(outer)) = (
                    item.id().filter(|id| !id.is_empty()),
                    event.item_id.as_ref().filter(|id| !id.is_empty()),
                ) && inner.as_str() != outer
                {
                    return Err(Self::invalid("conflicting item IDs in output event"));
                }
                let explicit = item.id().filter(|id| !id.is_empty()).cloned().or_else(|| {
                    event
                        .item_id
                        .filter(|id| !id.is_empty())
                        .map(ResponseItemId::from_server)
                });
                let added = event.kind == "response.output_item.added";
                let id = match explicit {
                    Some(id) => id,
                    None if added => ResponseItemId::with_suffix(prefix, uuid::Uuid::now_v7()),
                    None => {
                        let mut matches = self.open.iter().filter(|(_, open)| **open == kind);
                        match (matches.next(), matches.next()) {
                            (Some((id, _)), None) => id.clone(),
                            (None, None) => {
                                ResponseItemId::with_suffix(prefix, uuid::Uuid::now_v7())
                            }
                            _ => {
                                return Err(Self::invalid(
                                    "completion missing ID with multiple compatible open items",
                                ));
                            }
                        }
                    }
                };
                if let Some(open) = self.open.get(&id) {
                    if added {
                        return Err(Self::invalid("repeated open item ID"));
                    }
                    if *open != kind {
                        return Err(Self::invalid("completion item category mismatch"));
                    }
                }
                item.set_id(Some(id.clone()));
                if added {
                    self.open.insert(id, kind);
                    ResponseEvent::OutputItemAdded(item)
                } else {
                    self.open.remove(&id);
                    ResponseEvent::OutputItemDone(item)
                }
            }
            "response.output_text.delta" => {
                let Some(delta) = event.delta else {
                    return Ok(None);
                };
                let item_id = self.resolve(
                    event.item_id,
                    |kind| matches!(kind, ItemKind::Message(role) if role == "assistant"),
                )?;
                ResponseEvent::OutputTextDelta { item_id, delta }
            }
            "response.custom_tool_call_input.delta" => {
                let Some(delta) = event.delta else {
                    return Ok(None);
                };
                let item_id = self.resolve(event.item_id, |kind| {
                    matches!(kind,
                    ItemKind::CustomTool(call_id)
                    if event.call_id.as_ref().is_none_or(|expected| expected == call_id))
                })?;
                ResponseEvent::ToolCallInputDelta {
                    item_id,
                    call_id: event.call_id,
                    delta,
                }
            }
            "response.reasoning_summary_text.delta" => {
                let (Some(delta), Some(summary_index)) = (event.delta, event.summary_index) else {
                    return Ok(None);
                };
                let item_id =
                    self.resolve(event.item_id, |kind| matches!(kind, ItemKind::Reasoning))?;
                ResponseEvent::ReasoningSummaryDelta {
                    item_id,
                    delta,
                    summary_index,
                }
            }
            "response.reasoning_summary_text.done" => {
                let (Some(text), Some(summary_index)) = (event.text, event.summary_index) else {
                    return Ok(None);
                };
                let item_id =
                    self.resolve(event.item_id, |kind| matches!(kind, ItemKind::Reasoning))?;
                ResponseEvent::ReasoningSummaryDone {
                    item_id,
                    text,
                    summary_index,
                }
            }
            "response.reasoning_text.delta" => {
                let (Some(delta), Some(content_index)) = (event.delta, event.content_index) else {
                    return Ok(None);
                };
                let item_id =
                    self.resolve(event.item_id, |kind| matches!(kind, ItemKind::Reasoning))?;
                ResponseEvent::ReasoningContentDelta {
                    item_id,
                    delta,
                    content_index,
                }
            }
            "response.reasoning_summary_part.added" => {
                let Some(summary_index) = event.summary_index else {
                    return Ok(None);
                };
                let item_id =
                    self.resolve(event.item_id, |kind| matches!(kind, ItemKind::Reasoning))?;
                ResponseEvent::ReasoningSummaryPartAdded {
                    item_id,
                    summary_index,
                }
            }
            _ => unreachable!("non-item response event at item identity boundary"),
        };
        Ok(Some(output))
    }
}
