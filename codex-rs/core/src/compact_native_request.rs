//! One native compaction request: transport retries, and the provider's verdict on the input.

use std::sync::Arc;
use std::time::Duration;

use crate::Prompt;
use crate::client::ModelClientSession;
use crate::client::SamplingOptions;
use crate::client_common::ResponseEvent;
use crate::client_common::SamplingEvent;
use crate::client_common::SamplingStream;
use crate::client_common::SamplingUsage;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_retry::ResponsesStreamRequest;
use crate::responses_retry::ResponsesStreamRetryState;
use crate::responses_retry::handle_response_stream_error;
use crate::session::RequestEffortUsage;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::step_settings::ResolvedStepSettings;
use crate::session::turn_context::TurnContext;
use codex_api::AnthropicStopReason;
use codex_api::anthropic_stop_reason;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use codex_rollout_trace::CompactionTraceContext;
use http::StatusCode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Includes connection and header wait as well as streaming. Summarizing a large Claude context
/// may take minutes; cancellation still drops the attempt early.
const ATTEMPT_TIMEOUT: Duration = Duration::from_mins(10);
/// A compaction runs far longer than a sampling request, so it retries transport failures less.
const MAX_STREAM_RETRIES: u64 = 2;

/// Why an attempt produced no checkpoint.
pub(super) enum Miss {
    /// The provider refused the request outright, as invalid or oversized.
    Refused(CodexErr),
    /// The provider accepted the request but returned no usable checkpoint.
    Unfinished(CodexErr),
    /// Transport, account, quota, configuration or cancellation: a later retry may succeed.
    Failed(CodexErr),
}

impl Miss {
    fn of(error: CodexErr) -> Self {
        if refused(&error) {
            Self::Refused(error)
        } else {
            Self::Failed(error)
        }
    }
}

/// Whether the provider rejected the request itself rather than failed to serve it. Local
/// preconditions surface as prose `InvalidRequest`s; only the provider answers with an error body.
pub(super) fn refused(error: &CodexErr) -> bool {
    match error.details() {
        CodexErrorDetails::ContextWindowExceeded => true,
        CodexErrorDetails::InvalidRequest(body) => provider_error_message(body).is_some(),
        CodexErrorDetails::UnexpectedStatus(response) => matches!(
            response.status,
            StatusCode::BAD_REQUEST
                | StatusCode::PAYLOAD_TOO_LARGE
                | StatusCode::UNPROCESSABLE_ENTITY
        ),
        _ => false,
    }
}

/// The message of an Anthropic error body: `{"type":"error","error":{"type":..,"message":..}}`.
pub(super) fn provider_error_message(body: &str) -> Option<String> {
    let body = serde_json::from_str::<Value>(body).ok()?;
    if body["type"] != "error" {
        return None;
    }
    body["error"]["message"].as_str().map(str::to_owned)
}

pub(super) async fn request(
    sess: &Arc<Session>,
    step_context: &Arc<StepContext>,
    client_session: &mut ModelClientSession,
    prompt: &Arc<Prompt>,
    responses_metadata: &CodexResponsesMetadata,
    compaction_trace: &CompactionTraceContext,
) -> Result<(ResponseItem, String, Option<TokenUsage>), Miss> {
    let turn_context = &step_context.turn;
    let settings = &step_context.settings;
    let model_info = &settings.model_info;
    let trace = compaction_trace.inference_trace_context();
    let max_retries = turn_context
        .provider
        .info()
        .stream_max_retries()
        .min(MAX_STREAM_RETRIES);
    let mut retry_state = ResponsesStreamRetryState::default();
    let mut sampling = crate::hook_runtime::sampling::SamplingRequest::new(
        codex_hooks::SamplingKind::ForegroundCompaction,
    );
    let cancellation = &step_context.turn_cancellation;
    loop {
        Box::pin(sess.apply_pending_account())
            .await
            .map_err(|error| {
                Miss::Failed(
                    CodexErrorDetails::InvalidRequest(format!(
                        "Cannot apply account selection: {error}"
                    ))
                    .into(),
                )
            })?;
        let exchange = async {
            let effort = sess
                .reasoning_effort_for_request(settings, RequestEffortUsage::Compaction)
                .await;
            let send = client_session.open_model_response(
                prompt,
                model_info,
                &step_context.session_telemetry,
                SamplingOptions {
                    reasoning_effort: effort,
                    reasoning_summary: settings.reasoning_summary,
                    service_tier: settings.service_tier.clone(),
                },
                responses_metadata,
                &trace,
            );
            let stream = sampling
                .open(sess, step_context, &model_info.slug, cancellation, send)
                .await
                .map_err(Miss::of)?;
            collect(sess, turn_context, settings, stream, cancellation).await
        };
        let error = match tokio::time::timeout(ATTEMPT_TIMEOUT, exchange).await {
            Ok(Ok(output)) => return Ok(output),
            Ok(Err(Miss::Failed(error))) => error,
            Ok(Err(miss)) => return Err(miss),
            // The request may still finish upstream; it is not replayed at this size.
            Err(_) => {
                return Err(Miss::Unfinished(
                    CodexErrorDetails::Fatal(format!(
                        "Anthropic compaction returned no checkpoint within {} seconds",
                        ATTEMPT_TIMEOUT.as_secs()
                    ))
                    .into(),
                ));
            }
        };
        if sess.can_retry_after_account_handoff(&error) {
            continue;
        }
        handle_response_stream_error(
            &mut retry_state,
            max_retries,
            error,
            client_session,
            sess,
            step_context,
            ResponsesStreamRequest::RemoteCompactionV2,
        )
        .await
        .map_err(Miss::Failed)?;
    }
}

async fn collect(
    sess: &Session,
    turn_context: &TurnContext,
    settings: &ResolvedStepSettings,
    mut stream: SamplingStream,
    cancellation: &CancellationToken,
) -> Result<(ResponseItem, String, Option<TokenUsage>), Miss> {
    let mut checkpoints = Vec::new();
    while let Some(SamplingEvent { event, hook_output }) = stream.next_for_turn(cancellation).await
    {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                if let CodexErrorDetails::ResponseIncomplete(incomplete) = error.details() {
                    sess.record_incomplete_response_usage(
                        turn_context,
                        settings,
                        incomplete,
                        hook_output,
                    )
                    .await;
                }
                return Err(Miss::of(error));
            }
        };
        match event {
            ResponseEvent::OutputItemDone(item @ ResponseItem::NativeCompaction { .. }) => {
                checkpoints.push(item);
            }
            ResponseEvent::Completed {
                response_id,
                token_usage,
                usage_metadata,
                ..
            } => {
                sess.record_observed_response_completed(
                    turn_context,
                    &response_id,
                    token_usage.as_ref(),
                    usage_metadata.as_ref(),
                    /*thought*/ None,
                )
                .await;
                // Compaction is charged inference too. Record it at the completion boundary,
                // even when its checkpoint is rejected below; installing a checkpoint later
                // recomputes context occupancy, not cumulative usage.
                sess.record_token_usage_info(
                    turn_context,
                    settings,
                    SamplingUsage {
                        provider: token_usage.clone(),
                        charge: hook_output,
                    },
                    crate::session::TokenUsageContext::SpendingOnly,
                )
                .await
                .map_err(Miss::Failed)?;
                match usage_metadata.as_ref().and_then(anthropic_stop_reason) {
                    Some(AnthropicStopReason::ModelContextWindowExceeded) => {
                        return Err(Miss::Refused(
                            CodexErrorDetails::ContextWindowExceeded.into(),
                        ));
                    }
                    Some(AnthropicStopReason::MaxTokens) => {
                        return Err(Miss::Unfinished(
                            CodexErrorDetails::Fatal(
                                "Anthropic compaction reached its output limit before completing a checkpoint".into(),
                            )
                            .into(),
                        ));
                    }
                    Some(_) | None => {}
                }
                return match <[ResponseItem; 1]>::try_from(checkpoints) {
                    Ok([item]) => Ok((item, response_id, token_usage)),
                    Err(checkpoints) => Err(Miss::Unfinished(
                        CodexErrorDetails::Fatal(format!(
                            "Anthropic compaction returned {} checkpoints instead of one",
                            checkpoints.len()
                        ))
                        .into(),
                    )),
                };
            }
            _ => {}
        }
    }
    Err(Miss::Failed(
        CodexErrorDetails::Stream("Anthropic compaction stream closed before completion".into())
            .into(),
    ))
}

#[cfg(test)]
#[path = "compact_native_request_tests.rs"]
mod tests;
