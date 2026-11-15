//! The bill of each main-model request and what its step admitted into the context.
//!
//! A completed response leaves its step pending. The next completed request seals it: that
//! request's provider-reported input, less the marked request's input and the marked
//! response's re-sent output, is what the step's results and the arrivals after them added.
//! Membership is exact, the items between the two prompts. Local o200k counts only split the
//! delta among several shares, and run off the completion path.
//!
//! The mark lives in this process. A step an earlier process left pending can only be severed:
//! its request's prompt is gone.

use std::sync::Arc;

use codex_history::RolloutItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::step_admission::Admission;
use codex_protocol::step_admission::AdmissionPrecision;
use codex_protocol::step_admission::Arrival;
use codex_protocol::step_admission::ArrivalItem;
use codex_protocol::step_admission::ArrivalKind;
use codex_protocol::step_admission::SealedAdmission;
use codex_protocol::step_admission::StepAdmissionItem;
use codex_utils_output_truncation::count_o200k_arrival;
use codex_utils_output_truncation::count_o200k_text;

use super::Session;
use super::TurnContext;
use super::preamble::context_label;
use super::step_context::StepContext;
use crate::client_common::Prompt;
use crate::context::is_contextual_user_fragment;

/// The latest completed request, awaiting the next request's input to seal its step.
pub(crate) struct StepMark {
    item: StepAdmissionItem,
    turn_id: String,
    basis: StepBasis,
    announced: usize,
}

enum StepBasis {
    Measured(Baseline),
    // Replay proves where new arrivals begin, but not a cross-process token delta.
    Replayed(Option<codex_protocol::ResponseItemId>),
}

/// The marked request as this process sent it.
struct Baseline {
    model: String,
    history_version: u64,
    prompt_len: usize,
    /// The response's whole output, which the next request re-sends.
    output_tokens: u64,
}

impl Baseline {
    /// Whether `prompt` extends the marked request's context unchanged.
    fn extended_by(&self, prompt_len: usize, model: &str, history_version: u64) -> bool {
        self.history_version == history_version
            && self.model == model
            && self.prompt_len <= prompt_len
    }
}

impl StepMark {
    pub(super) fn orphaned(
        rollout: &[RolloutItem],
        thread_id: codex_protocol::ThreadId,
    ) -> Option<Self> {
        let mut orphan: Option<Self> = None;
        let mut tip = None;
        for entry in rollout {
            match entry {
                RolloutItem::ResponseItem(item) => {
                    if let Some(id) = item.id() {
                        tip = Some(id.clone());
                    }
                }
                RolloutItem::Compacted(_) => tip = None,
                RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
                    thread_id: owner,
                    turn_id,
                    item,
                    ..
                })) if *owner == thread_id => match item {
                    TurnItem::StepAdmission(item) if item.admission == Admission::Pending => {
                        orphan = Some(Self {
                            item: item.clone(),
                            turn_id: turn_id.clone(),
                            basis: StepBasis::Replayed(tip.clone()),
                            announced: 0,
                        });
                    }
                    TurnItem::StepAdmission(item)
                        if orphan.as_ref().is_some_and(|mark| mark.item.id == item.id) =>
                    {
                        orphan = None;
                    }
                    TurnItem::Arrival(item) => {
                        if let Some(mark) = orphan
                            .as_mut()
                            .filter(|mark| mark.item.response_id == item.response_id)
                            && let Some(index) = item
                                .id
                                .rsplit(':')
                                .next()
                                .and_then(|index| index.parse::<usize>().ok())
                        {
                            mark.announced = mark.announced.max(index + 1);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        orphan
    }
}

/// The items a step admitted, before the delta is split among them.
#[derive(Debug)]
struct Admitted {
    results: Option<Vec<String>>,
    arrivals: Vec<Incoming>,
}

impl Default for Admitted {
    fn default() -> Self {
        Self {
            results: Some(Vec::new()),
            arrivals: Vec::new(),
        }
    }
}

/// Text-only admission evidence; request attachments are released before streaming.
pub(super) struct PreparedStepAdmission {
    prompt_len: usize,
    previous: Option<CapturedAdmission>,
}

struct CapturedAdmission {
    response_id: String,
    admitted: Admitted,
}

/// The calls and assistant messages one response produced, observed as its items complete.
#[derive(Debug, Default)]
pub(super) struct ResponseMembers {
    calls: Vec<String>,
    messages: Vec<String>,
}

impl ResponseMembers {
    pub(super) fn observe(&mut self, item: &ResponseItem) {
        match item {
            ResponseItem::FunctionCall { call_id, .. }
            | ResponseItem::CustomToolCall { call_id, .. } => self.calls.push(call_id.clone()),
            ResponseItem::LocalShellCall { call_id, .. }
            | ResponseItem::ToolSearchCall { call_id, .. } => {
                self.calls.extend(call_id.iter().cloned());
            }
            ResponseItem::Message {
                role, id: Some(id), ..
            } if role == "assistant" => self.messages.push(id.to_string()),
            _ => {}
        }
    }
}

#[derive(Debug)]
struct Incoming {
    kind: ArrivalKind,
    source_index: usize,
    source: codex_protocol::step_admission::ArrivalSource,
    label: String,
}

#[derive(Debug, PartialEq, Eq)]
struct Apportionment {
    shares: Vec<u64>,
    released: u64,
    precision: AdmissionPrecision,
}

impl Session {
    pub(super) async fn prepare_step_admission(
        &self,
        step_context: &StepContext,
        prompt: &Prompt,
    ) -> PreparedStepAdmission {
        let previous = {
            let state = self.state.lock().await;
            state.step_mark.as_ref().and_then(|mark| match &mark.basis {
                StepBasis::Measured(baseline)
                    if baseline.extended_by(
                        prompt.input.len(),
                        &step_context.settings.model_info.slug,
                        state.history.history_version(),
                    ) =>
                {
                    Some((mark.item.response_id.clone(), baseline.prompt_len))
                }
                StepBasis::Measured(_) | StepBasis::Replayed(_) => None,
            })
        };
        PreparedStepAdmission {
            prompt_len: prompt.input.len(),
            previous: previous.map(|(response_id, start)| CapturedAdmission {
                response_id,
                admitted: admit(&prompt.input[start..]),
            }),
        }
    }

    /// Leaves this response's step pending and seals the previous step with this request's
    /// input. The previous step is severed when the context was rewritten, the model changed
    /// or this request reported no usage.
    pub(super) async fn record_step_admission(
        self: &Arc<Self>,
        step_context: &StepContext,
        prompt: PreparedStepAdmission,
        response_id: &str,
        members: ResponseMembers,
        usage: Option<&TokenUsage>,
        reported_reasoning_tokens: Option<u64>,
    ) {
        let turn_context = &step_context.turn;
        let model = step_context.settings.model_info.slug.as_str();
        let read_tokens = usage.map(|usage| u64::try_from(usage.input_tokens).unwrap_or(0));
        let (previous, pending, history_version) = {
            let mut state = self.state.lock().await;
            let history_version = state.history.history_version();
            let mark = usage.zip(read_tokens).map(|(usage, read_tokens)| {
                let output_tokens = u64::try_from(usage.output_tokens).unwrap_or(0);
                let thinking_tokens =
                    reported_reasoning_tokens.filter(|thinking| *thinking <= output_tokens);
                StepMark {
                    item: StepAdmissionItem {
                        id: format!("step:{response_id}"),
                        response_id: response_id.to_string(),
                        completed_at_ms: crate::turn_timing::now_unix_timestamp_ms(),
                        read_tokens,
                        thinking_tokens,
                        output_tokens: output_tokens - thinking_tokens.unwrap_or(0),
                        call_ids: members.calls,
                        message_ids: members.messages,
                        admission: Admission::Pending,
                    },
                    turn_id: turn_context.sub_id.clone(),
                    announced: 0,
                    basis: StepBasis::Measured(Baseline {
                        model: model.to_string(),
                        history_version,
                        prompt_len: prompt.prompt_len,
                        output_tokens,
                    }),
                }
            });
            let pending = mark.as_ref().map(|mark| mark.item.clone());
            (
                std::mem::replace(&mut state.step_mark, mark),
                pending,
                history_version,
            )
        };
        if let Some(item) = pending {
            self.emit_step_item(
                turn_context,
                &turn_context.sub_id,
                TurnItem::StepAdmission(item),
            )
            .await;
        }
        let Some(mut previous) = previous else {
            return;
        };
        let measured = read_tokens
            .zip(match &previous.basis {
                StepBasis::Measured(baseline) => Some(baseline),
                StepBasis::Replayed(_) => None,
            })
            .filter(|(_, baseline)| baseline.extended_by(prompt.prompt_len, model, history_version))
            .zip(prompt.previous)
            .filter(|(_, captured)| captured.response_id == previous.item.response_id);
        let Some(((read_tokens, baseline), captured)) = measured else {
            previous.item.admission = Admission::Severed;
            self.emit_step_item(
                turn_context,
                &previous.turn_id,
                TurnItem::StepAdmission(previous.item),
            )
            .await;
            return;
        };
        let received = i128::from(read_tokens)
            - i128::from(previous.item.read_tokens)
            - i128::from(baseline.output_tokens);
        let admitted = captured.admitted;
        let session = Arc::clone(self);
        let turn_context = Arc::clone(turn_context);
        // Tokenizing large or pathological text must not hold up the turn.
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || seal(previous, received, admitted)).await {
                Ok((turn_id, item)) => {
                    session
                        .emit_step_item(&turn_context, &turn_id, TurnItem::StepAdmission(item))
                        .await;
                }
                Err(error) => tracing::warn!(%error, "step admission measurement failed"),
            }
        });
    }

    /// Announces what arrived after the previous step's results as this request reads it.
    pub(super) async fn announce_step_arrivals(&self, step_context: &StepContext, prompt: &Prompt) {
        let (response_id, arrivals) = {
            let mut state = self.state.lock().await;
            let history_version = state.history.history_version();
            let model = step_context.settings.model_info.slug.as_str();
            let Some(mark) = state.step_mark.as_mut() else {
                return;
            };
            let start = match &mark.basis {
                StepBasis::Measured(baseline)
                    if baseline.extended_by(prompt.input.len(), model, history_version) =>
                {
                    Some(baseline.prompt_len)
                }
                StepBasis::Replayed(Some(tip)) => prompt
                    .input
                    .iter()
                    .rposition(|item| item.id() == Some(tip))
                    .map(|index| index + 1),
                _ => None,
            };
            let Some(start) = start else { return };
            let arrivals = admit(&prompt.input[start..]).arrivals;
            let announced = mark.announced;
            mark.announced = announced.max(arrivals.len());
            let fresh = arrivals.into_iter().enumerate().skip(announced);
            (
                mark.item.response_id.clone(),
                fresh
                    .map(|(index, incoming)| {
                        (
                            index,
                            arrival_placement(&prompt.input, start + incoming.source_index),
                            incoming,
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let announced_at_ms = crate::turn_timing::now_unix_timestamp_ms();
        for (index, placement, incoming) in arrivals {
            if incoming.kind == ArrivalKind::User {
                continue;
            }
            let item = ArrivalItem {
                id: arrival_id(&response_id, index),
                response_id: response_id.clone(),
                announced_at_ms,
                kind: incoming.kind,
                source: incoming.source,
                label: incoming.label,
                text: None,
                placement,
            };
            self.emit_step_item(
                &step_context.turn,
                &step_context.turn.sub_id,
                TurnItem::Arrival(item),
            )
            .await;
        }
    }

    async fn emit_step_item(&self, turn_context: &TurnContext, turn_id: &str, item: TurnItem) {
        self.send_event(
            turn_context,
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: self.thread_id,
                turn_id: turn_id.to_string(),
                item,
                started_at_ms: None,
                content_token_count: None,
                completed_at_ms: crate::turn_timing::now_unix_timestamp_ms(),
            }),
        )
        .await;
    }
}

/// Human transcript alias only; history preparation remains the owner of model item IDs.
pub(super) fn user_response_item_id(
    items: &[codex_history::ResponseItemEnvelope],
) -> Option<codex_protocol::ResponseItemId> {
    items.iter().find_map(|envelope| {
        let ResponseItem::Message {
            id,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = &envelope.item
        else {
            return None;
        };
        metadata
            .content_item_kinds
            .as_ref()
            .filter(|kinds| {
                !kinds.is_empty() && kinds.iter().all(|kind| kind.0.starts_with("user."))
            })
            .and(id.clone())
    })
}

fn arrival_placement(
    items: &[ResponseItem],
    prompt_index: usize,
) -> Option<codex_protocol::step_admission::ArrivalPlacement> {
    use codex_protocol::step_admission::ArrivalPlacement;
    use codex_protocol::step_admission::ArrivalSide;
    let user_id = |item: &ResponseItem| match item {
        ResponseItem::Message {
            id, role, content, ..
        } if role == "user" && !content.iter().all(is_contextual_user_fragment) => Some(id.clone()),
        _ => None,
    };
    if let Some(user_response_item_id) = items[prompt_index + 1..].iter().find_map(user_id) {
        return user_response_item_id.map(|user_response_item_id| ArrivalPlacement {
            user_response_item_id,
            side: ArrivalSide::Before,
        });
    }
    items[..prompt_index]
        .iter()
        .rev()
        .find_map(user_id)
        .flatten()
        .map(|user_response_item_id| ArrivalPlacement {
            user_response_item_id,
            side: ArrivalSide::After,
        })
}

/// One arrival's id, shared by its announcement and its step's seal.
fn arrival_id(response_id: &str, index: usize) -> String {
    format!("arrival:{response_id}:{index}")
}

/// Sorts the items between two prompts into the step's results and its arrivals. The
/// response's own items count as its re-sent output.
fn admit(items: &[ResponseItem]) -> Admitted {
    let mut admitted = Admitted::default();
    for (source_index, item) in items.iter().enumerate() {
        match item {
            ResponseItem::Message { role, .. } if role == "assistant" => {}
            ResponseItem::Message { role, content, .. } => {
                let blocks: Vec<String> = content
                    .iter()
                    .filter_map(|content| match content {
                        ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                            Some(text.clone())
                        }
                        ContentItem::InputImage { .. } | ContentItem::InputAudio { .. } => None,
                    })
                    .collect();
                let label = context_label(&blocks.join("\n"));
                let kind = if role == "user" && !content.iter().all(is_contextual_user_fragment) {
                    ArrivalKind::User
                } else {
                    ArrivalKind::Injection
                };
                admitted.arrivals.push(Incoming {
                    source_index,
                    kind,
                    source: codex_protocol::step_admission::ArrivalSource::Captured { blocks },
                    label,
                });
            }
            ResponseItem::AgentMessage {
                author, content, ..
            } => admitted.arrivals.push(Incoming {
                source_index,
                kind: ArrivalKind::InterAgent,
                source: content
                    .iter()
                    .map(|part| match part {
                        codex_protocol::models::AgentMessageInputContent::InputText { text } => {
                            Some(text.clone())
                        }
                        codex_protocol::models::AgentMessageInputContent::EncryptedContent {
                            ..
                        } => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .map_or(
                        codex_protocol::step_admission::ArrivalSource::Unavailable,
                        |blocks| codex_protocol::step_admission::ArrivalSource::Captured { blocks },
                    ),
                label: author.clone(),
            }),
            ResponseItem::AdditionalTools { tools, .. } => {
                admitted.arrivals.push(Incoming {
                    source_index,
                    kind: ArrivalKind::Injection,
                    label: "Additional tools".to_string(),
                    source: serde_json::to_string(tools).map_or(
                        codex_protocol::step_admission::ArrivalSource::Unavailable,
                        |text| codex_protocol::step_admission::ArrivalSource::ToolDeclarations {
                            text,
                        },
                    ),
                });
            }
            ResponseItem::ConfigurationUpdate { .. } => admitted.arrivals.push(Incoming {
                source_index,
                kind: ArrivalKind::Injection,
                label: "Configuration update".to_string(),
                source: codex_protocol::step_admission::ArrivalSource::Unavailable,
            }),
            ResponseItem::Compaction { .. }
            | ResponseItem::NativeCompaction { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::CompactionTrigger {} => admitted.arrivals.push(Incoming {
                source_index,
                kind: ArrivalKind::Injection,
                label: "Compaction".to_string(),
                source: codex_protocol::step_admission::ArrivalSource::Unavailable,
            }),
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                if let Some(results) = &mut admitted.results {
                    let text = match &output.body {
                        FunctionCallOutputBody::Text(text) => Some(vec![text.clone()]),
                        FunctionCallOutputBody::ContentItems(items) => items
                            .iter()
                            .map(|item| match item {
                                FunctionCallOutputContentItem::InputText { text } => {
                                    Some(text.clone())
                                }
                                FunctionCallOutputContentItem::InputImage { .. }
                                | FunctionCallOutputContentItem::InputAudio { .. }
                                | FunctionCallOutputContentItem::EncryptedContent { .. } => None,
                            })
                            .collect::<Option<Vec<_>>>(),
                    };
                    match text {
                        Some(text) => results.extend(text),
                        None => admitted.results = None,
                    }
                }
            }
            ResponseItem::ToolSearchOutput { tools, .. } => {
                if let Some(results) = &mut admitted.results {
                    results.push(serde_json::to_string(tools).unwrap_or_default());
                }
            }
            ResponseItem::Reasoning { .. }
            | ResponseItem::FunctionCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::RejectedToolCall { .. }
            | ResponseItem::Other => {}
        }
    }
    admitted
}

/// Counts the step's shares and splits what it received among them.
fn seal(mut mark: StepMark, received: i128, admitted: Admitted) -> (String, StepAdmissionItem) {
    let local = std::iter::once(admitted.results.as_ref().map(|results| {
        results.iter().fold(0u64, |total, text| {
            total.saturating_add(count_o200k_text(text))
        })
    }))
    .chain(
        admitted
            .arrivals
            .iter()
            .map(|arrival| count_o200k_arrival(&arrival.source)),
    )
    .collect::<Vec<_>>();
    let Apportionment {
        shares,
        released,
        precision,
    } = apportion(received, &local);
    mark.item.admission = Admission::Sealed(SealedAdmission {
        self_tokens: shares[0],
        arrivals: admitted
            .arrivals
            .into_iter()
            .zip(&shares[1..])
            .enumerate()
            .map(|(index, (incoming, tokens))| Arrival {
                id: arrival_id(&mark.item.response_id, index),
                kind: incoming.kind,
                label: incoming.label,
                tokens: *tokens,
            })
            .collect(),
        released_tokens: released,
        precision,
    });
    (mark.turn_id, mark.item)
}

/// Splits `received` among shares whose local counts are `local`; the first share is the
/// step's own results. A delta below the local lower bound means context left during the step.
fn apportion(received: i128, local: &[Option<u64>]) -> Apportionment {
    let known = local.iter().flatten().sum::<u64>();
    let Ok(received) = u64::try_from(received) else {
        return disturbed(received, known, local);
    };
    if received < known {
        return disturbed(i128::from(received), known, local);
    }
    let mut shares = vec![0; local.len()];
    let contributors = local.iter().filter(|share| **share != Some(0)).count();
    if contributors <= 1 {
        let index = local
            .iter()
            .position(|share| *share != Some(0))
            .unwrap_or(0);
        shares[index] = received;
        return Apportionment {
            shares,
            released: 0,
            precision: AdmissionPrecision::Exact,
        };
    }
    let unknown = local.iter().filter(|share| share.is_none()).count() as u64;
    if unknown > 0 {
        // Measured shares keep their counts; the opaque ones split what remains.
        let rest = received - known;
        let mut extra = rest % unknown;
        for (share, count) in shares.iter_mut().zip(local) {
            *share = count.unwrap_or_else(|| {
                let bonus = u64::from(extra > 0);
                extra -= bonus;
                rest / unknown + bonus
            });
        }
    } else {
        for (share, count) in shares.iter_mut().zip(local) {
            let count = count.unwrap_or(0);
            *share = u64::try_from(u128::from(count) * u128::from(received) / u128::from(known))
                .unwrap_or(u64::MAX);
        }
        let first = local
            .iter()
            .position(|share| *share != Some(0))
            .unwrap_or(0);
        shares[first] += received - shares.iter().sum::<u64>();
    }
    Apportionment {
        shares,
        released: 0,
        precision: AdmissionPrecision::Apportioned,
    }
}

fn disturbed(received: i128, known: u64, local: &[Option<u64>]) -> Apportionment {
    Apportionment {
        shares: local.iter().map(|share| share.unwrap_or(0)).collect(),
        released: u64::try_from(i128::from(known) - received).unwrap_or(u64::MAX),
        precision: AdmissionPrecision::Disturbed,
    }
}

#[cfg(test)]
#[path = "step_admission_tests.rs"]
mod tests;
