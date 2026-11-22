use std::sync::Arc;

use super::Miss;
use super::collect;
use crate::client_common::ResponseEvent;
use crate::client_common::SamplingStream;
use codex_protocol::ResponseUsageMetadata;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RawResponseCompletedEvent;
use codex_protocol::protocol::TokenUsage;
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

fn stream(event: CodexResult<ResponseEvent>) -> SamplingStream {
    let (tx, rx_event) = tokio::sync::mpsc::channel(/*buffer*/ 1);
    tx.try_send(event.into()).unwrap();
    SamplingStream {
        routing_affinity: None,
        rx_event,
        interrupt: None,
        consumer_dropped: CancellationToken::new(),
        settlement_started: Arc::default(),
    }
}

#[tokio::test]
async fn unusable_completions_record_usage_and_name_their_rung_escalation() {
    // A completion without a checkpoint is charged inference: its usage must survive
    // even though the ladder discards it. Its classification decides whether a smaller
    // request (refused, unfinished) or no retry at all (failed) follows.
    let usage = TokenUsage {
        input_tokens: 40,
        output_tokens: 7,
        total_tokens: 47,
        ..Default::default()
    };
    let completed = |stop_reason: &str| {
        let metadata = ResponseUsageMetadata {
            reported_reasoning_tokens: None,
            amount: None,
            metadata: Some(serde_json::json!({
                "provider": "anthropic",
                "stop_reason": stop_reason,
                "usage": {"input_tokens": 40, "output_tokens": 7},
            })),
        };
        (
            ResponseEvent::Completed {
                response_id: format!("msg_{stop_reason}"),
                token_usage: Some(usage.clone()),
                usage_metadata: Some(metadata.clone()),
                reasoning_removals: None,
                end_turn: Some(false),
            },
            RawResponseCompletedEvent {
                thought: None,
                response_id: format!("msg_{stop_reason}"),
                token_usage: Some(usage.clone()),
                usage_metadata: Some(metadata),
            },
        )
    };
    let (sess, context, rx) =
        Box::pin(crate::session::tests::make_session_and_context_with_rx()).await;
    for (stop_reason, refused) in [
        ("model_context_window_exceeded", true),
        ("max_tokens", false),
        ("end_turn", false),
    ] {
        let (event, recorded) = completed(stop_reason);
        let miss = collect(
            &sess,
            &context,
            &context.initial_settings,
            stream(Ok(event)),
            &CancellationToken::new(),
        )
        .await
        .expect_err("a completion without one checkpoint is no checkpoint");
        assert!(
            matches!(
                (&miss, refused),
                (Miss::Refused(error), true)
                    if matches!(error.details(), CodexErrorDetails::ContextWindowExceeded)
            ) || matches!((&miss, refused), (Miss::Unfinished(_), false)),
            "{stop_reason}"
        );
        let EventMsg::RawResponseCompleted(observed) = rx.recv().await.unwrap().msg else {
            panic!("usage must be recorded before {stop_reason} is rejected");
        };
        assert_eq!(
            serde_json::to_value(observed).unwrap(),
            serde_json::to_value(recorded).unwrap()
        );
    }
    let mut total = TokenUsage::default();
    for _ in 0..3 {
        total.add_assign(&usage);
    }
    assert_eq!(
        sess.token_usage_info().await.unwrap().total_token_usage,
        total
    );

    // Unknown stream outcomes are the transport's, not the history's: no smaller rung.
    let miss = collect(
        &sess,
        &context,
        &context.initial_settings,
        stream(Err(CodexErr::Fatal(
            "Native Anthropic stream ended with an unknown request outcome".into(),
        ))),
        &CancellationToken::new(),
    )
    .await
    .expect_err("unknown native compaction outcome must remain a failure");
    assert!(matches!(miss, Miss::Failed(_)));
}
