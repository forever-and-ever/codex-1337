//! Review accounting with and without client events, including wall-clock skew.

use super::*;
use codex_protocol::approvals::NetworkApprovalProtocol;
use codex_protocol::protocol::TokenUsage;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;

#[test_case::test_case(
    Some(GuardianAssessmentAction::NetworkAccess {
        target: "example.com".to_string(),
        host: "example.com".to_string(),
        protocol: NetworkApprovalProtocol::Https,
        port: 443,
    }),
    GuardianReviewedAction::NetworkAccess { protocol: NetworkApprovalProtocol::Https, port: 443 };
    "with client event"
)]
#[test_case::test_case(None, GuardianReviewedAction::NativeToolCall {}; "without client event")]
fn review_completion_uses_one_monotonic_duration(
    action: Option<GuardianAssessmentAction>,
    reviewed_action: GuardianReviewedAction,
) {
    let has_client_event = action.is_some();
    let mut report = ReviewReport::new(ReviewMetadata {
        thread_id: "thread".to_string(),
        turn_id: "turn".to_string(),
        turn_trigger: None,
        review_id: "review".to_string(),
        target_item_id: None,
        plugin_id: None,
        script_path: None,
        approval_request_source: GuardianApprovalRequestSource::MainTurn,
        reviewed_action,
        action,
        review_reason: GuardianReviewReason::Policy,
        timeout: Some(crate::REVIEW_TIMEOUT),
        model_context: ModelInvocationContext {
            model_slug: "test-model".to_string(),
            reasoning_effort: None,
        },
    });
    report.tracking.started_at_ms = 1_654_073_807_000;
    if let Some(event) = &mut report.started {
        event.started_at_ms =
            i64::try_from(report.tracking.started_at_ms).expect("start timestamp");
    }
    assert_eq!(report.started_event().is_some(), has_client_event);
    report.started_instant = Instant::now() - Duration::from_millis(456);
    let model: ModelInfo = serde_json::from_value(json!({
        "slug": "test-model",
        "display_name": "Test model",
        "supported_reasoning_levels": [],
        "shell_type": "disabled",
        "visibility": "none",
        "supported_in_api": false,
        "priority": 0,
        "support_verbosity": false,
        "truncation_policy": {"mode": "tokens", "limit": 1000},
        "experimental_supported_tools": []
    }))
    .expect("test model");

    let before = report.started_instant.elapsed();
    let (completed, elapsed) = report.complete(
        GuardianReviewOutcome::Error(crate::GuardianReviewError::Timeout),
        &model,
        /*require_guardian*/ true,
        GuardianReviewAnalyticsResult {
            attempt_count: 2,
            token_usage: Some(TokenUsage {
                input_tokens: 10,
                total_tokens: 10,
                ..Default::default()
            }),
            ..GuardianReviewAnalyticsResult::without_session()
        },
        /*completed_at_ms*/ 1_792_141_011_000,
    );
    let after = report.started_instant.elapsed();
    assert!((before..=after).contains(&elapsed));
    assert_eq!(
        completed
            .event
            .as_ref()
            .map(|event| (event.started_at_ms, event.completed_at_ms)),
        has_client_event.then_some((1_654_073_807_000, Some(1_792_141_011_000)))
    );
}

#[test_case::test_case(Some("automation"), "automation"; "automation")]
#[test_case::test_case(Some("mcp_event"), "automation"; "webhook")]
#[test_case::test_case(Some("automation_cron_scheduled"), "automation"; "scheduled")]
#[test_case::test_case(Some("automation_cron_run_now"), "automation"; "run_now")]
#[test_case::test_case(Some("automation_heartbeat_scheduled"), "automation"; "heartbeat_scheduled")]
#[test_case::test_case(Some("automation_heartbeat_run_now"), "automation"; "heartbeat_run_now")]
#[test_case::test_case(Some("composer"), "other"; "user")]
#[test_case::test_case(Some("arbitrary client supplied value"), "other"; "bounded_labels")]
#[test_case::test_case(Some(""), "unknown"; "empty")]
#[test_case::test_case(None, "unknown"; "missing")]
fn review_report_bounds_turn_origin(turn_trigger: Option<&str>, expected_origin: &str) {
    let report = ReviewReport::new(ReviewMetadata {
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        turn_trigger: turn_trigger.map(str::to_owned),
        review_id: "review".into(),
        target_item_id: None,
        plugin_id: None,
        script_path: None,
        approval_request_source: GuardianApprovalRequestSource::MainTurn,
        reviewed_action: GuardianReviewedAction::NativeToolCall {},
        action: None,
        review_reason: GuardianReviewReason::Policy,
        model_context: ModelInvocationContext {
            model_slug: "test-model".into(),
            reasoning_effort: None,
        },
        timeout: None,
    });
    assert_eq!(report.turn_origin, expected_origin);
}
