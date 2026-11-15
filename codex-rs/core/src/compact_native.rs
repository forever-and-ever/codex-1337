//! Anthropic signed compaction.
//!
//! Compaction is the escape from a history the provider no longer serves, so it must not share
//! that history's fate. Each rung asks the provider to summarize less: the whole history, then
//! its tail, then only the recent user messages and the last reply. When the provider accepts
//! even that small request yet returns no checkpoint, the thread resets to the same material
//! without a model call. Every concession is announced.

use crate::context::CompactionContinuation;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::Prompt;
use crate::client::ModelClientSession;
use crate::compact::CompactedHistoryMetadata;
use crate::compact::CompactionAnalyticsDetails;
use crate::compact::CompactionWindow;
use crate::compact::SUMMARY_PREFIX;
use crate::compact::build_compacted_history;
use crate::compact::build_compaction_replacement_history;
use crate::compact::collect_annotated_user_messages;
use crate::compact_remote_history::trim_function_call_history_to_fit_context_window;
use crate::context::UserGoalUpdate;
use crate::context::world_state::WorldState;
use crate::context_manager::estimate_item_token_count;
use crate::responses_metadata::CompactionTurnMetadata;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn::get_last_assistant_message_from_turn;
use crate::session::turn_context::TurnContext;
use codex_api::AnthropicRepair;
use codex_api::AnthropicSalvage;
use codex_api::salvage_anthropic_history;
use codex_history::ResponseItemEnvelope;
use codex_model_provider::RemoteCompactionSupport;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::items::ContextCompactionItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::InputModality;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::WarningEvent;
use codex_rollout_trace::CompactionCheckpointTracePayload;
use codex_rollout_trace::CompactionTraceContext;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::truncate_text;
use tracing::warn;

#[path = "compact_native_request.rs"]
mod request;
use request::Miss;
use request::provider_error_message;
use request::refused;
use request::request;

/// The tail rung's conversation budget when the model declares no context window.
const UNKNOWN_WINDOW_TAIL_TOKENS: usize = 100_000;
const RESET_LAST_MESSAGE_TOKENS: usize = 4_000;
const REASON_CHARS: usize = 240;
const RESET_NOTICE: &str = "The earlier conversation could not be summarized, so it was reset. Its tool calls, tool results and intermediate work are gone; the user's recent messages above were kept. Re-establish any state you need, for example by re-reading files, before continuing.";

/// How much of the history one compaction request covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rung {
    Whole,
    /// Signed checkpoints and the most recent quarter of the context window.
    Tail,
    /// The recent user messages and the last reply, as text. Small and free of tool traffic,
    /// so its refusal can only blame the request's tools, instructions or parameters.
    Recent,
}

/// A signed checkpoint, with the repairs its input needed.
struct Checkpoint {
    item: ResponseItem,
    response_id: String,
    token_usage: Option<TokenUsage>,
    repairs: Vec<AnthropicRepair>,
}

/// Whether a native sampling failure lies with the history, which compaction replaces: the
/// provider refused the request, or the encoder rejected a history that salvage would repair.
/// Configuration faults also surface as invalid prompts; they must not cost the history.
pub(crate) async fn history_rejected(
    sess: &Session,
    turn_context: &TurnContext,
    error: &CodexErr,
) -> bool {
    if turn_context
        .provider
        .remote_compaction_for_model(turn_context.model_info())
        != RemoteCompactionSupport::AnthropicSigned
    {
        return false;
    }
    if refused(error) {
        return true;
    }
    if !matches!(error.details(), CodexErrorDetails::InvalidPrompt { .. }) {
        return false;
    }
    let modalities = &turn_context.model_info().input_modalities;
    let input = sess
        .clone_history()
        .await
        .for_prompt(modalities, sess.services.image_store.as_ref())
        .await;
    !salvage_anthropic_history(input, modalities)
        .repairs
        .is_empty()
}

pub(crate) async fn run_native_compaction(
    sess: &Arc<Session>,
    step_context: &Arc<StepContext>,
    client_session: Option<&mut ModelClientSession>,
    replacement_step_context: &StepContext,
    world_state: &WorldState,
    compaction_metadata: CompactionTurnMetadata,
    analytics_details: &mut CompactionAnalyticsDetails,
) -> CodexResult<()> {
    let turn_context = &step_context.turn;
    let compaction = ContextCompactionItem::new();
    let compaction_trace = sess.services.rollout_thread_trace.compaction_trace_context(
        turn_context.sub_id.as_str(),
        compaction.id.as_str(),
        turn_context.model_info().slug.as_str(),
        turn_context.provider.info().name.as_str(),
    );
    let compaction = TurnItem::ContextCompaction(compaction);
    sess.emit_turn_item_started(turn_context, &compaction).await;
    let mut owned_client_session = None;
    let client_session = match client_session {
        Some(client_session) => client_session,
        None => owned_client_session.insert(sess.services.model_client.new_session()),
    };

    let mut history = sess.clone_history().await;
    // Whichever rung succeeds replaces this snapshot; appends made meanwhile survive.
    let source = history.clone().into_shared_annotated_items();
    history = history.for_compaction(&world_state.render_full().0);
    let mut base_instructions = sess.get_prompt_base_instructions().await;
    if let Some(text) = history.take_base_instructions() {
        base_instructions.text = text;
    }
    let (_, estimated_deleted_tokens) = trim_function_call_history_to_fit_context_window(
        &mut history,
        turn_context.as_ref(),
        turn_context.model_context_window(),
        &base_instructions,
    );
    if estimated_deleted_tokens > 0 {
        let max_local_deleted_tokens = sess
            .clone_history()
            .await
            .estimated_tokens_since_last_usage();
        analytics_details.active_context_tokens_before = analytics_details
            .active_context_tokens_before
            .map(|tokens| {
                tokens.saturating_sub(estimated_deleted_tokens.min(max_local_deleted_tokens))
            });
    }
    let trace_input_history = compaction_trace
        .is_enabled()
        .then(|| history.raw_items().cloned().collect::<Vec<_>>());
    let mut whole = history
        .for_prompt(
            &turn_context.model_info().input_modalities,
            sess.services.image_store.as_ref(),
        )
        .await;
    sess.services
        .executed_tool_calls
        .attach_to_compaction_prompt(&mut whole);
    let tail = tail(
        &whole,
        turn_context
            .model_context_window()
            .and_then(|window| usize::try_from(window / 4).ok())
            .unwrap_or(UNKNOWN_WINDOW_TAIL_TOKENS),
    );
    let recent = reset_history(&source)
        .0
        .into_iter()
        .map(ResponseItemEnvelope::into_item)
        .collect();

    let mut rejections = Vec::new();
    for (rung, input) in [
        (Rung::Whole, Some(whole)),
        (Rung::Tail, tail),
        (Rung::Recent, Some(recent)),
    ] {
        let Some(input) = input else {
            continue;
        };
        let checkpoint = match Box::pin(attempt(
            sess,
            step_context,
            client_session,
            &compaction_trace,
            compaction_metadata,
            base_instructions.clone(),
            rung,
            input,
        ))
        .await
        {
            Ok(checkpoint) => checkpoint,
            Err(Miss::Failed(error)) => return Err(error),
            // The smallest, cleanest request was refused: no reset would change the tools,
            // instructions or parameters at fault.
            Err(Miss::Refused(error)) if rung == Rung::Recent => return Err(error),
            Err(Miss::Refused(error) | Miss::Unfinished(error)) => {
                warn!(?rung, %error, "native compaction rung rejected");
                rejections.push(error);
                continue;
            }
        };
        let concession = concession(rung, &checkpoint.repairs, &rejections);
        Box::pin(install_checkpoint(
            sess,
            turn_context,
            replacement_step_context,
            world_state,
            &compaction_trace,
            analytics_details,
            source,
            trace_input_history,
            checkpoint,
        ))
        .await?;
        if let Some(message) = concession {
            sess.send_event(turn_context, EventMsg::Warning(WarningEvent { message }))
                .await;
        }
        sess.emit_turn_item_completed(turn_context, compaction)
            .await;
        return Ok(());
    }
    install_reset(
        sess,
        turn_context,
        replacement_step_context,
        world_state,
        source,
    )
    .await?;
    let message = format!(
        "Claude could not compact this conversation ({}). It was reset to your recent messages and Claude's last reply; earlier tool results and work are gone.",
        describe_all(&rejections)
    );
    sess.send_event(turn_context, EventMsg::Warning(WarningEvent { message }))
        .await;
    sess.emit_turn_item_completed(turn_context, compaction)
        .await;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "one attempt of the ladder carries its whole context"
)]
async fn attempt(
    sess: &Arc<Session>,
    step_context: &Arc<StepContext>,
    client_session: &mut ModelClientSession,
    compaction_trace: &CompactionTraceContext,
    compaction_metadata: CompactionTurnMetadata,
    base_instructions: BaseInstructions,
    rung: Rung,
    input: Vec<ResponseItem>,
) -> Result<Checkpoint, Miss> {
    let turn_context = &step_context.turn;
    let modalities: &[InputModality] = match rung {
        Rung::Whole | Rung::Tail => &turn_context.model_info().input_modalities,
        Rung::Recent => &[InputModality::Text],
    };
    let AnthropicSalvage { items, repairs } = salvage_anthropic_history(input, modalities);
    let tool_router = &step_context.tool_router;
    let prompt = Prompt {
        input: items,
        tools: tool_router.model_visible_specs(),
        parallel_tool_calls: true,
        base_instructions,
        output_schema: None,
        output_schema_strict: true,
        cyber_access_program: turn_context.cyber_access_program,
        prompt_cache_ttl: turn_context.prompt_cache_ttl,
    };
    let mut responses_metadata = sess
        .compaction_responses_metadata(turn_context.as_ref(), compaction_metadata)
        .await;
    responses_metadata.tool_namespaces_info = tool_router.tool_namespaces_info().cloned();
    let output = Box::pin(request(
        sess,
        step_context,
        client_session,
        &prompt,
        &responses_metadata,
        compaction_trace,
    ))
    .await;
    let (item, response_id, token_usage) = output?;
    Ok(Checkpoint {
        item,
        response_id,
        token_usage,
        repairs,
    })
}

/// The signed checkpoints and the most recent conversation within `budget` tokens, opened at a
/// user message where one exists so that it does not begin inside a tool batch. `None` when
/// that would keep everything, since the whole history was already refused.
fn tail(input: &[ResponseItem], budget: usize) -> Option<Vec<ResponseItem>> {
    let mut used = 0usize;
    let mut start = input.len();
    for (index, item) in input.iter().enumerate().rev() {
        used = used
            .saturating_add(usize::try_from(estimate_item_token_count(item)).unwrap_or(usize::MAX));
        if used > budget {
            break;
        }
        start = index;
    }
    let start = input[start..]
        .iter()
        .position(|item| matches!(item, ResponseItem::Message { role, .. } if role == "user"))
        .map_or(start, |offset| start + offset);
    let tail = input
        .iter()
        .enumerate()
        .filter(|(index, item)| {
            *index >= start || matches!(item, ResponseItem::NativeCompaction { .. })
        })
        .map(|(_, item)| item.clone())
        .collect::<Vec<_>>();
    (tail.len() < input.len()).then_some(tail)
}

/// The recent user messages and a notice carrying the last reply: the material both the
/// `Recent` rung and a reset preserve. Returns it with the notice's text.
fn reset_history(source: &[ResponseItemEnvelope]) -> (Vec<ResponseItemEnvelope>, String) {
    let mut notice = RESET_NOTICE.to_owned();
    if let Some(message) =
        get_last_assistant_message_from_turn(source.iter().map(|envelope| &envelope.item))
    {
        notice.push_str("\n\nThe last assistant message before the reset:\n");
        notice.push_str(&truncate_text(
            &message,
            TruncationPolicy::Tokens(RESET_LAST_MESSAGE_TOKENS),
        ));
    }
    let summary = format!("{SUMMARY_PREFIX}\n{notice}");
    let history = build_compacted_history(
        Vec::new(),
        &collect_annotated_user_messages(source),
        &summary,
    );
    (history, summary)
}

#[expect(
    clippy::too_many_arguments,
    reason = "installation joins the ladder's shared state"
)]
async fn install_checkpoint(
    sess: &Arc<Session>,
    turn_context: &TurnContext,
    replacement_step_context: &StepContext,
    world_state: &WorldState,
    compaction_trace: &CompactionTraceContext,
    analytics_details: &mut CompactionAnalyticsDetails,
    source: Arc<Vec<ResponseItemEnvelope>>,
    trace_input_history: Option<Vec<ResponseItem>>,
    checkpoint: Checkpoint,
) -> CodexResult<()> {
    let Checkpoint {
        item,
        response_id,
        token_usage,
        repairs: _,
    } = checkpoint;
    if let Some(token_usage) = token_usage {
        analytics_details.active_context_tokens_before = Some(token_usage.input_tokens);
        analytics_details.compaction_summary_tokens = Some(token_usage.output_tokens);
        analytics_details.cached_input_tokens = Some(token_usage.cached_input_tokens);
        analytics_details.cache_write_input_tokens = Some(token_usage.cache_write_input_tokens);
    }
    analytics_details.retained_image_count = Some(0);
    let (initial_context, world_state_baseline) = Box::pin(build_compaction_replacement_history(
        sess.as_ref(),
        replacement_step_context,
        world_state,
        Vec::new(),
        CompactionContinuation::capture(source.iter().map(|envelope| &envelope.item))?,
    ))
    .await;
    // Anthropic requires its signed checkpoint before conversation messages, and initial
    // context can contain user-role AGENTS.md and environment fragments.
    let history = std::iter::once(ResponseItemEnvelope::new(item))
        .chain(initial_context)
        .collect();
    sess.replace_compacted_history(
        history,
        replacement_step_context.to_turn_context_item(),
        world_state_baseline,
        BTreeSet::default(),
        CompactedHistoryMetadata {
            input_goal_ids: UserGoalUpdate::message_ids(
                source.iter().map(|envelope| &envelope.item),
            ),
            source_history: Some(source),
            message: String::new(),
            window: CompactionWindow::Advance,
            compaction_response_id: Some(response_id),
            compaction_model_hash: turn_context.model_info().comp_hash.clone(),
            reviewer_compaction_hash: None,
        },
    )
    .await?;
    if let Some(input_history) = trace_input_history.as_deref() {
        let replacement_history = sess
            .clone_history()
            .await
            .raw_items()
            .cloned()
            .collect::<Vec<_>>();
        compaction_trace.record_installed_with_protocol(
            &CompactionCheckpointTracePayload {
                input_history,
                replacement_history: &replacement_history,
            },
            codex_rollout_trace::ModelRequestProtocol::Messages,
        );
    }
    sess.recompute_token_usage(turn_context).await;
    Ok(())
}

/// Replaces the history with its reset material, without a model call.
async fn install_reset(
    sess: &Arc<Session>,
    turn_context: &TurnContext,
    replacement_step_context: &StepContext,
    world_state: &WorldState,
    source: Arc<Vec<ResponseItemEnvelope>>,
) -> CodexResult<()> {
    let (history, summary) = reset_history(&source);
    let (history, world_state_baseline) = Box::pin(build_compaction_replacement_history(
        sess.as_ref(),
        replacement_step_context,
        world_state,
        history,
        CompactionContinuation::capture(source.iter().map(|envelope| &envelope.item))?,
    ))
    .await;
    sess.replace_compacted_history(
        history,
        replacement_step_context.to_turn_context_item(),
        world_state_baseline,
        BTreeSet::default(),
        CompactedHistoryMetadata {
            input_goal_ids: UserGoalUpdate::message_ids(
                source.iter().map(|envelope| &envelope.item),
            ),
            source_history: Some(source),
            message: summary,
            window: CompactionWindow::Advance,
            compaction_response_id: None,
            compaction_model_hash: turn_context.model_info().comp_hash.clone(),
            reviewer_compaction_hash: None,
        },
    )
    .await?;
    sess.recompute_token_usage(turn_context).await;
    Ok(())
}

/// The warning owed for a checkpoint that cost repairs or earlier context.
fn concession(rung: Rung, repairs: &[AnthropicRepair], rejections: &[CodexErr]) -> Option<String> {
    let mut parts = Vec::new();
    match rung {
        Rung::Whole => {}
        Rung::Tail => parts.push(format!(
            "Claude could not compact the whole conversation ({}); only its most recent part was summarized, and older context was dropped.",
            describe_all(rejections)
        )),
        Rung::Recent => parts.push(format!(
            "Claude could not compact the conversation ({}); only your recent messages and Claude's last reply were summarized, and earlier tool results and work were dropped.",
            describe_all(rejections)
        )),
    }
    if !repairs.is_empty() {
        let mut counts = BTreeMap::<AnthropicRepair, usize>::new();
        for repair in repairs {
            match repair {
                AnthropicRepair::Closed(calls) => {
                    *counts.entry(AnthropicRepair::Closed(0)).or_default() += calls;
                }
                repair => *counts.entry(repair.clone()).or_default() += 1,
            }
        }
        let repairs = counts
            .into_iter()
            .map(|(repair, count)| match repair {
                AnthropicRepair::Omitted(reason) => format!("omitted {count} item(s): {reason}"),
                AnthropicRepair::Degraded(reason) => {
                    format!("replaced media in {count} item(s): {reason}")
                }
                AnthropicRepair::Closed(_) => format!("answered {count} unanswered tool call(s)"),
                AnthropicRepair::Shed(items) => format!(
                    "replaced media in the {items} oldest item(s) to fit the request size limit"
                ),
            })
            .collect::<Vec<_>>()
            .join("; ");
        parts.push(format!(
            "Repaired history that Claude cannot accept before compacting it: {repairs}."
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn describe_all(errors: &[CodexErr]) -> String {
    errors.iter().map(describe).collect::<Vec<_>>().join("; ")
}

fn describe(error: &CodexErr) -> String {
    let text = match error.details() {
        CodexErrorDetails::InvalidRequest(body) => {
            provider_error_message(body).unwrap_or_else(|| error.to_string())
        }
        _ => error.to_string(),
    };
    match text.char_indices().nth(REASON_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}
