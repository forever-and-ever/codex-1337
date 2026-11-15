//! Measure only accepted model-history results, never raw or nested code-mode tool returns.

use super::Session;
use super::TurnContext;
use crate::context_manager::ContextManager;
use crate::tools::code_mode::CaptureOrigin;
use codex_protocol::ToolName;
use codex_protocol::items::FunctionCallOutputItem;
use codex_protocol::items::ToolOutputTokensItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_rollout::RolloutItem;

pub(super) struct CapturedOutput {
    id: String,
    call_id: String,
    body: FunctionCallOutputBody,
    /// Whether the accepted body was pure text. `body` keeps only the text parts, so a
    /// mixed-media body records no count instead of a misleading text subtotal.
    countable: bool,
    tool: Option<ToolName>,
    owner: Option<codex_protocol::code_mode::CodeModeOwner>,
    origin: CaptureOrigin,
}

pub(super) fn capture(history: &ContextManager, previous_len: usize) -> Vec<CapturedOutput> {
    history
        .raw_items()
        .skip(previous_len)
        .filter_map(|item| {
            let (call_id, body, name) = match item {
                ResponseItem::FunctionCallOutput {
                    call_id: Some(call_id),
                    output,
                    ..
                } => (call_id, &output.body, None),
                ResponseItem::CustomToolCallOutput {
                    call_id,
                    output,
                    name,
                    ..
                } => (
                    call_id,
                    &output.body,
                    name.as_ref().filter(|name| name.as_str() == "exec"),
                ),
                _ => return None,
            };
            // notify() retains its tool name even if compaction removed the original exec.
            let tool = name.map(ToolName::plain).or_else(|| {
                history.raw_items().rev().find_map(|call| match call {
                    ResponseItem::CustomToolCall {
                        call_id: id,
                        name,
                        namespace,
                        ..
                    } if id == call_id => Some(ToolName::new(namespace.clone(), name)),
                    ResponseItem::FunctionCall {
                        call_id: id,
                        name,
                        namespace,
                        ..
                    } if id == call_id => Some(ToolName::new(namespace.clone(), name)),
                    _ => None,
                })
            });
            let (body, countable) = match body {
                FunctionCallOutputBody::Text(text) => {
                    (FunctionCallOutputBody::Text(text.clone()), true)
                }
                FunctionCallOutputBody::ContentItems(items) => (
                    FunctionCallOutputBody::ContentItems(
                        items
                            .iter()
                            .filter(|item| {
                                matches!(item, FunctionCallOutputContentItem::InputText { .. })
                            })
                            .cloned()
                            .collect(),
                    ),
                    items.iter().all(|item| {
                        matches!(item, FunctionCallOutputContentItem::InputText { .. })
                    }),
                ),
            };
            Some(CapturedOutput {
                id: item.id()?.to_string(),
                call_id: call_id.clone(),
                body,
                countable,
                tool,
                owner: None,
                // notify() explicitly retains the exec name; ordinary returned
                // outputs have no name. Do not infer this from script text/body.
                origin: if name.is_some() {
                    CaptureOrigin::Notification
                } else {
                    CaptureOrigin::Returned
                },
            })
        })
        .collect()
}

impl Session {
    pub(super) async fn record_tool_outputs(
        &self,
        turn: &TurnContext,
        mut outputs: Vec<CapturedOutput>,
    ) {
        if outputs.is_empty() {
            return;
        }
        let thread_id = self.thread_id;
        let turn_id = turn.sub_id.clone();
        for output in &mut outputs {
            output.owner = self
                .services
                .code_mode_service
                .transcript_owner(&output.call_id);
            if output.tool.is_none()
                && let Some(owner) = &output.owner
            {
                output.tool = Some(ToolName::plain(if output.call_id == owner.exec_call_id {
                    crate::tools::code_mode::PUBLIC_TOOL_NAME
                } else {
                    crate::tools::code_mode::WAIT_TOOL_NAME
                }));
            }
        }
        // Nested tool rows describe execution, not the text JavaScript chose to emit.
        // Publish that returned batch before asynchronous counting so its footer has
        // a stable transcript position even when counting finishes after the turn.
        for output in &outputs {
            if let Some(tool) = &output.tool {
                // The audit surface observes text, not duplicate multi-megabyte media.
                // Preserve captured text blocks and their IDs without reserializing results.
                self.send_event_with_owner(
                    turn,
                    EventMsg::ItemCompleted(ItemCompletedEvent {
                        thread_id,
                        turn_id: turn_id.clone(),
                        item: TurnItem::FunctionCallOutput(FunctionCallOutputItem {
                            id: output.id.clone(),
                            call_id: Some(output.call_id.clone()),
                            name: tool.name.clone(),
                            namespace: tool.namespace.clone(),
                            output: output.body.clone(),
                        }),
                        started_at_ms: None,
                        content_token_count: None,
                        completed_at_ms: super::now_unix_timestamp_ms(),
                    }),
                    output.owner.clone(),
                )
                .await;
            }
        }
        // Several notify()/returned batches can share a call. Every accepted body and
        // deferred measurement already copied its descriptor before retiring that lease.
        for output in &outputs {
            self.services
                .code_mode_service
                .release_transcript_capture(&output.call_id, output.origin);
        }
        // Display and invocation ownership are provider-neutral, even without a tokenizer.
        if turn.config.model_provider_id != "openai"
            && !self.services.model_client.uses_claude_subscription()
        {
            return;
        }
        let live_thread = self.live_thread().cloned();
        let tx_event = self.get_tx_event();
        // These are observations, not execution barriers. In particular cold vocabulary
        // initialization must not hold up sampling, tool dispatch, or interruption.
        tokio::spawn(async move {
            let counts = tokio::task::spawn_blocking(move || {
                outputs
                    .into_iter()
                    .map(|output| {
                        (
                            ToolOutputTokensItem {
                                // A code-mode call can return multiple outputs through notify(). Each
                                // measurement needs its own persisted identity for idempotent replay.
                                id: format!("tool-output-tokens:{}", output.id),
                                call_id: output.call_id,
                                tokens: output
                                    .countable
                                    .then(|| {
                                        codex_utils_output_truncation::count_o200k_text_tokens(
                                            &output.body,
                                        )
                                    })
                                    .flatten(),
                            },
                            output.owner,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .await;
            let Ok(counts) = counts else {
                tracing::warn!("tool output token measurement failed");
                return;
            };
            for (item, owner) in counts {
                let native = EventMsg::ItemCompleted(ItemCompletedEvent {
                    thread_id,
                    turn_id: turn_id.clone(),
                    item: TurnItem::ToolOutputTokens(item),
                    started_at_ms: None,
                    content_token_count: None,
                    completed_at_ms: super::now_unix_timestamp_ms(),
                });
                let msg = match owner {
                    Some(owner) => EventMsg::CodeModeOwned(
                        codex_protocol::code_mode::CodeModeOwned::new(owner, Box::new(native)),
                    ),
                    None => native,
                };
                if let Some(live_thread) = &live_thread
                    && let Err(error) = live_thread
                        .append_items(&[RolloutItem::EventMsg(msg.clone())])
                        .await
                {
                    tracing::debug!(%error, "could not persist tool output measurement");
                }
                // No execution lifecycle, legacy fanout, or parent-notification side effects.
                // Counts may arrive after turn completion; their turn and call identities survive.
                let _ = tx_event
                    .send(Event {
                        id: turn_id.clone(),
                        msg,
                    })
                    .await;
            }
        });
    }
}
