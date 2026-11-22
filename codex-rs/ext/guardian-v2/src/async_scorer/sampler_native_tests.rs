//! Native decisions must not inherit Responses' first-delta approval shortcut.

use super::*;
use codex_protocol::ResponseUsageMetadata;
use pretty_assertions::assert_eq;
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::test]
async fn native_classification_requires_complete_structured_output() {
    let complete = json!({"provider":"anthropic","stop_reason":"end_turn"});
    let metadata = |raw| {
        Some(ResponseUsageMetadata {
            reported_reasoning_tokens: None,
            amount: None,
            metadata: Some(raw),
        })
    };
    for (output, terminal, usage_metadata, expected) in [
        (
            r#"{"risk":"low"}"#,
            Some(true),
            metadata(complete.clone()),
            Some("low"),
        ),
        (
            r#"{"risk":"high"}"#,
            Some(true),
            metadata(complete.clone()),
            Some("high"),
        ),
        (
            r#"{"risk":"low"}"#,
            Some(false),
            metadata(complete.clone()),
            None,
        ),
        (r#"{"risk":"low"}"#, None, metadata(complete.clone()), None),
        (
            r#"{"risk":"low"} garbage"#,
            Some(true),
            metadata(complete.clone()),
            None,
        ),
        (r#"{"risk":"medium"}"#, Some(true), metadata(complete), None),
        (
            r#"{"risk":"low"}"#,
            Some(true),
            metadata(json!({"provider":"anthropic","stop_reason":"refusal"})),
            None,
        ),
        (
            r#"{"risk":"low"}"#,
            Some(true),
            metadata(json!({"provider":"anthropic","stop_reason":"max_tokens"})),
            None,
        ),
        (
            r#"{"risk":"low"}"#,
            Some(true),
            metadata(json!({"provider":"anthropic","stop_reason":17})),
            None,
        ),
        (
            r#"{"risk":"low"}"#,
            Some(true),
            metadata(json!({"provider":"anthropic"})),
            None,
        ),
        (
            r#"{"risk":"low"}"#,
            Some(true),
            metadata(json!({"stop_reason":"end_turn"})),
            None,
        ),
        (
            r#"{"risk":"low"}"#,
            Some(true),
            metadata(json!({"provider":"openai","stop_reason":"end_turn"})),
            None,
        ),
        (r#"{"risk":"low"}"#, Some(true), None, None),
    ] {
        let (tx, rx_event) = mpsc::channel(/*buffer*/ 4);
        let stream = ResponseStream {
            rx_event,
            upstream_request_id: None,
            routing_affinity: None,
            interrupt: None,
        };
        let scored = Arc::new(AtomicBool::new(/*v*/ false));
        let mut classification =
            tokio::spawn(decision(stream, /*metrics*/ None, Arc::clone(&scored)));
        // Even a full valid low object is provisional until message_stop.
        tx.send(Ok(ResponseEvent::OutputTextDelta {
            item_id: codex_protocol::ResponseItemId::from_server("classifier".into()),
            delta: output.into(),
        }))
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(/*millis*/ 20), &mut classification)
                .await
                .is_err()
        );
        assert!(!scored.load(Ordering::Relaxed));
        if let Some(end_turn) = terminal {
            tx.send(Ok(ResponseEvent::Completed {
                response_id: "classifier".into(),
                token_usage: None,
                usage_metadata,
                end_turn: Some(end_turn),
                reasoning_removals: None,
            }))
            .await
            .unwrap();
        }
        drop(tx);
        let result = classification.await.unwrap();
        assert_eq!(
            (result.ok().as_deref(), scored.load(Ordering::Relaxed)),
            (expected, expected.is_some()),
        );
    }
}
