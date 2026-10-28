//! Admitted complete inputs count once per sampling step, including retry preparation.

use super::*;
use std::sync::Arc;

#[tokio::test]
#[test_case::test_case(128_000; "admitted")]
#[test_case::test_case(1; "rejected")]
async fn request_budget_marks_only_admitted_steps_across_retry_preparation(window: i64) {
    let (session, mut turn) = crate::session::tests::make_session_and_context().await;
    Arc::make_mut(&mut turn.config).model_context_window = Some(window);
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let input = ["retained history", "new review"]
        .map(|role| {
            serde_json::from_value(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("{role}: {}", "x".repeat(48_000)),
                }],
            }))
            .expect("valid user message")
        })
        .to_vec();
    // Admission belongs to the step, not the transport retry.
    for _ in 0..2 {
        let step = session
            .capture_step_context(
                Arc::clone(&turn),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut prompt = crate::session::turn::build_prompt(
            input.clone(),
            &step,
            session.get_prompt_base_instructions().await,
            /*incremental_tools*/ false,
        );
        let metadata = session
            .responses_metadata(
                &step,
                crate::responses_metadata::CodexResponsesRequestKind::Turn,
            )
            .await;
        // The retry loop invokes Guardian preparation again on the same step.
        for _ in 0..2 {
            let result = prepare_prompt(&session, &mut prompt, &step, &metadata).await;
            if window > 1 {
                result.unwrap();
                assert!(step.extension_data.get::<ObservedRequest>().is_some());
            } else {
                assert!(step.extension_data.get::<ObservedRequest>().is_none());
                assert!(matches!(
                    result.unwrap_err().details(),
                    codex_protocol::error::CodexErrorDetails::ContextWindowExceeded
                ));
            }
        }
    }
}
