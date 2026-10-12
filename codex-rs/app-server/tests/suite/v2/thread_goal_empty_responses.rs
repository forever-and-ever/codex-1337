//! Exercises goal reminder retention and the empty-response breaker through the public API.

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ThreadGoalGetResponse;
use codex_app_server_protocol::ThreadGoalSetResponse;
use codex_app_server_protocol::ThreadGoalStatus;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_features::Feature;
use core_test_support::mock_server;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

#[derive(Clone, Copy)]
enum ReminderContext {
    Retained,
    Compacted,
    WithoutUpdatePlan,
}

#[test_case::test_case(None, ReminderContext::Retained; "empty")]
#[test_case::test_case(Some("final_answer"), ReminderContext::Retained; "draft")]
#[test_case::test_case(Some("commentary"), ReminderContext::Retained; "commentary")]
#[test_case::test_case(Some("tool"), ReminderContext::Retained; "tool")]
#[test_case::test_case(None, ReminderContext::Compacted; "compacted")]
#[test_case::test_case(None, ReminderContext::WithoutUpdatePlan; "without_update_plan")]
#[tokio::test]
async fn empty_goal_continuations_block_after_three_without_activity(
    recovery: Option<&str>,
    context: ReminderContext,
) -> Result<()> {
    let turns = if recovery.is_some() {
        6
    } else if matches!(context, ReminderContext::Compacted) {
        // The second turn includes a nonempty compaction summary, followed by
        // the three empty turns that exercise the existing breaker.
        5
    } else {
        3
    };
    let mut scripts = Vec::new();
    for turn in 1..=turns {
        let mut id = format!("response-{turn}");
        let mut events = vec![responses::ev_response_created(&id)];
        if turn == 3
            && let Some(recovery) = recovery
        {
            if recovery == "tool" {
                events.push(responses::ev_function_call("get-goal", "get_goal", "{}"));
                events.push(responses::ev_completed(&id));
                scripts.push(responses::sse(events));
                id.push_str("-after-tool");
                events = vec![responses::ev_response_created(&id)];
            } else {
                let mut added = responses::ev_assistant_message("progress", "");
                added["type"] = json!("response.output_item.added");
                events.push(added);
                events.push(responses::ev_output_text_delta("Useful progress"));
                let mut event = responses::ev_assistant_message("progress", "Useful progress");
                event["item"]["phase"] = json!(recovery);
                events.push(event);
            }
        }
        let mut empty_final = responses::ev_assistant_message(&format!("empty-final-{turn}"), "");
        empty_final["item"]["phase"] = json!("final_answer");
        events.push(empty_final);
        events.push(responses::ev_completed_with_tokens(
            &id,
            if turn == 1 && matches!(context, ReminderContext::Compacted) {
                60_000
            } else {
                100
            },
        ));
        scripts.push(responses::sse(events));
        if turn == 1 && matches!(context, ReminderContext::Compacted) {
            scripts.push(responses::sse(vec![
                responses::ev_assistant_message("summary", "GOAL_COMPACTION_SUMMARY"),
                responses::ev_completed_with_tokens("compacted", /*total_tokens*/ 200),
            ]));
        }
    }
    let server = responses::start_mock_server().await;
    mock_server::run(&[&server], async {
    let requests = responses::mount_sse_sequence(&server, scripts).await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .enable_feature(Feature::Goals)
        .with_root_config("model_auto_compact_token_limit = 50000\nmodel_post_turn_compact_threshold_percent = 0\ncompact_prompt = \"GOAL_COMPACTION_PROMPT\"")
        .with_root_config(if matches!(context, ReminderContext::WithoutUpdatePlan) {
            "tools.update_plan.enabled = false"
        } else {
            "tools.update_plan.enabled = true"
        })
        .write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized()
        .await?;
    let request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = mcp.read_response(request).await?;
    let request = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(json!({"threadId": thread.id, "objective": "Finish <the> & update_plan work"})),
        )
        .await?;
    let _: ThreadGoalSetResponse = mcp.read_response(request).await?;

    for turn in 1..=turns {
        if turn == 3 && matches!(recovery, Some("final_answer" | "commentary")) {
            let delta: serde_json::Value = timeout(
                Duration::from_secs(30),
                mcp.read_notification("item/agentMessage/delta"),
            )
            .await??;
            assert_eq!(json!("Useful progress"), delta["delta"]);
        }
        let completed: TurnCompletedNotification = timeout(
            Duration::from_secs(30),
            mcp.read_notification("turn/completed"),
        )
        .await??;
        assert_eq!(TurnStatus::Completed, completed.turn.status);
        assert_eq!(None, completed.turn.error);
    }
    let request = mcp
        .send_raw_request("thread/goal/get", Some(json!({"threadId": thread.id})))
        .await?;
    let result: ThreadGoalGetResponse = mcp.read_response(request).await?;
    assert_eq!(
        ThreadGoalStatus::Blocked,
        result.goal.expect("goal exists").status
    );
    server.verify().await;

    let mut requests = requests.requests();
    if matches!(context, ReminderContext::Compacted) {
        // The continuation was admitted before pre-turn compaction removed its
        // earlier rules. The next sampling request must restore them, not rely
        // on a decision made by the idle continuation scheduler.
        assert!(requests[1].body_contains_text("GOAL_COMPACTION_PROMPT"));
        requests.remove(1);
        assert!(requests[1].body_contains_text("GOAL_COMPACTION_SUMMARY"));
    }
    assert_eq!(
        requests.len(),
        turns + usize::from(recovery == Some("tool"))
    );
    for (index, request) in requests.iter().enumerate() {
        let texts = request.message_input_texts("user");
        let rules = texts
            .iter()
            .filter(|text| text.contains("Completion audit:"))
            .collect::<Vec<_>>();
        assert_eq!(rules.len(), 1, "request {index}");
        assert_eq!(
            rules[0].contains("If update_plan is available"),
            !matches!(context, ReminderContext::WithoutUpdatePlan),
        );
        let reminders = texts
            .iter()
            .filter(|text| text.contains("Continue working toward the active thread goal."))
            .collect::<Vec<_>>();
        let latest = reminders.last().expect("goal reminder");
        assert!(
            latest.contains("<objective>\nFinish &lt;the&gt; &amp; update_plan work\n</objective>")
        );
        assert!(latest.contains(
            "The full goal-continuation instructions earlier in this context still apply."
        ));
        assert!(
            reminders
                .iter()
                .all(|text| !text.contains("Completion audit:"))
        );
        if index == 0 {
            assert!(latest.contains("Tokens used: 0\n"));
        } else {
            assert!(!latest.contains("Tokens used: 0\n"));
        }
    }
    Ok(())
    })
    .await
}
