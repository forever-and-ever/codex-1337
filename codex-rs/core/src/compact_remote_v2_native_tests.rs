use super::collect_compaction_output;
use crate::client_common::ResponseEvent;
use crate::client_common::ResponseStream;
use crate::compact_model_fallback::should_retry_with_current_model;
use codex_model_provider_info::ModelProviderAdapter;
use codex_protocol::ResponseUsageMetadata;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RawResponseCompletedEvent;
use codex_protocol::protocol::TokenUsage;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn native_context_limit_records_usage_without_accepting_a_checkpoint() {
    // A Completed event must retain charged usage while still rejecting the
    // context-limit stop; treating it as success would continue identical history.
    let usage = TokenUsage {
        input_tokens: 40,
        output_tokens: 7,
        total_tokens: 47,
        ..Default::default()
    };
    let metadata = ResponseUsageMetadata {
        amount: None,
        metadata: Some(serde_json::json!({
            "provider": "anthropic",
            "stop_reason": "model_context_window_exceeded",
            "usage": {"input_tokens": 40, "output_tokens": 7},
        })),
    };
    let (tx, rx_event) = tokio::sync::mpsc::channel(/*buffer*/ 1);
    tx.try_send(Ok(ResponseEvent::Completed {
        response_id: "msg_context_limit".into(),
        token_usage: Some(usage.clone()),
        usage_metadata: Some(metadata.clone()),
        end_turn: Some(false),
    }))
    .unwrap();
    drop(tx);
    let stream = ResponseStream {
        rx_event,
        interrupt: None,
        consumer_dropped: CancellationToken::new(),
    };
    let (sess, mut context, rx) = crate::session::tests::make_session_and_context_with_rx().await;
    let context_mut = Arc::get_mut(&mut context).unwrap();
    let mut provider = context_mut.provider.info().clone();
    provider.adapter = ModelProviderAdapter::ClaudeSubscription;
    provider.requires_openai_auth = false;
    provider.supports_websockets = false;
    provider.base_url = None;
    context_mut.provider =
        codex_model_provider::create_model_provider(provider, /*auth_manager*/ None);
    assert!(matches!(
        collect_compaction_output(&sess, &context, stream).await,
        Err(error) if matches!(error.details(), CodexErrorDetails::ContextWindowExceeded)
    ));
    let EventMsg::RawResponseCompleted(completed) = rx.recv().await.unwrap().msg else {
        panic!("native completion usage must be recorded before the context-limit error");
    };
    assert_eq!(
        serde_json::to_value(completed).unwrap(),
        serde_json::to_value(RawResponseCompletedEvent {
            response_id: "msg_context_limit".into(),
            token_usage: Some(usage.clone()),
            usage_metadata: Some(metadata.clone()),
        })
        .unwrap()
    );

    // A token ceiling is terminal, but known charged usage must still survive.
    let mut output_limit_metadata = metadata;
    output_limit_metadata.metadata.as_mut().unwrap()["stop_reason"] =
        serde_json::json!("max_tokens");
    let (tx, rx_event) = tokio::sync::mpsc::channel(/*buffer*/ 1);
    tx.try_send(Ok(ResponseEvent::Completed {
        response_id: "msg_output_limit".into(),
        token_usage: Some(usage.clone()),
        usage_metadata: Some(output_limit_metadata.clone()),
        end_turn: Some(false),
    }))
    .unwrap();
    drop(tx);
    let error = collect_compaction_output(
        &sess,
        &context,
        ResponseStream {
            rx_event,
            interrupt: None,
            consumer_dropped: CancellationToken::new(),
        },
    )
    .await
    .err()
    .expect("native output-limit completion must reject a checkpoint");
    assert!(matches!(
        error.details(),
        codex_protocol::error::CodexErrorDetails::Fatal(_)
    ));
    assert!(!should_retry_with_current_model(&error));
    let EventMsg::RawResponseCompleted(completed) = rx.recv().await.unwrap().msg else {
        panic!("output-limit usage must precede the terminal failure");
    };
    assert_eq!(
        serde_json::to_value(completed).unwrap(),
        serde_json::to_value(RawResponseCompletedEvent {
            response_id: "msg_output_limit".into(),
            token_usage: Some(usage.clone()),
            usage_metadata: Some(output_limit_metadata),
        })
        .unwrap()
    );

    // The durable response ledger alone is insufficient: status-line totals
    // must include charged compaction even when no checkpoint can be installed.
    let mut total = usage.clone();
    total.add_assign(&usage);
    assert_eq!(
        sess.token_usage_info().await.unwrap().total_token_usage,
        total
    );

    // Unknown native outcomes propagate through collection and must not trigger
    // the previous-model -> current-model fallback, even after a downshift.
    let (tx, rx_event) = tokio::sync::mpsc::channel(/*buffer*/ 1);
    tx.try_send(Err(CodexErr::Fatal(
        "Native Anthropic stream ended with an unknown request outcome".into(),
    )))
    .unwrap();
    drop(tx);
    let error = collect_compaction_output(
        &sess,
        &context,
        ResponseStream {
            rx_event,
            interrupt: None,
            consumer_dropped: CancellationToken::new(),
        },
    )
    .await
    .err()
    .expect("unknown native compaction outcome must remain a failure");
    assert!(!should_retry_with_current_model(&error));
    assert!(matches!(
        error.details(),
        codex_protocol::error::CodexErrorDetails::Fatal(_)
    ));
}
