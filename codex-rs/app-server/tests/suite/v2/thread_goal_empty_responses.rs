//! Exercises goal reminder retention and the no-activity breaker through the public API.

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::ConfigEdit;
use codex_app_server_protocol::ConfigWriteResponse;
use codex_app_server_protocol::HookEventName;
use codex_app_server_protocol::HookSource;
use codex_app_server_protocol::HooksListParams;
use codex_app_server_protocol::HooksListResponse;
use codex_app_server_protocol::MergeStrategy;
use codex_app_server_protocol::ThreadGoalSetResponse;
use codex_app_server_protocol::ThreadGoalStatus;
use codex_app_server_protocol::ThreadGoalUpdatedNotification;
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
#[test_case::test_case(Some("interrupt"), ReminderContext::Retained; "interrupt")]
#[tokio::test]
async fn empty_goal_continuations_block_after_three_without_activity(
    recovery: Option<&str>,
    context: ReminderContext,
) -> Result<()> {
    let turns = if recovery == Some("interrupt") {
        4
    } else if recovery.is_some() {
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
        if recovery == Some("interrupt") {
            events.push(responses::ev_assistant_message(
                "progress",
                "Ready for the next step",
            ));
            events.push(responses::ev_completed_with_tokens(
                &id, /*total_tokens*/ 330_000,
            ));
            scripts.push(responses::sse(events));
            break;
        }
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
    let mut config = MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .enable_feature(Feature::Goals)
        .with_root_config(if recovery == Some("interrupt") {
            "model_auto_compact_token_limit = 200000\nmodel_post_turn_compact_threshold_percent = 0"
        } else {
            "model_auto_compact_token_limit = 50000\nmodel_post_turn_compact_threshold_percent = 0\ncompact_prompt = \"GOAL_COMPACTION_PROMPT\""
        })
        .with_root_config(if matches!(context, ReminderContext::WithoutUpdatePlan) {
            "tools.update_plan.enabled = false"
        } else {
            "tools.update_plan.enabled = true"
        })
;
    if recovery == Some("interrupt") {
        config = config
            .enable_feature(Feature::CodexHooks)
            .with_extra_config(
                r#"
[[hooks.PreCompact]]
selector = { type = "event_value", pattern = "auto" }
[[hooks.PreCompact.hooks]]
type = "command"
command = """printf '%s' '{"continue":false}'"""
command_windows = """Write-Output '{"continue":false}'"""
"#,
            );
    }
    config.write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized()
        .await?;
    if recovery == Some("interrupt") {
        let list = mcp
            .send_hooks_list_request(HooksListParams {
                cwds: vec![codex_home.path().to_path_buf()],
                ..Default::default()
            })
            .await?;
        let HooksListResponse { data } = mcp.read_response(list).await?;
        let hook = data[0]
            .hooks
            .iter()
            .find(|hook| {
                hook.source == HookSource::User && hook.event_name == HookEventName::PreCompact
            })
            .expect("user config should register the pre-compact hook");
        let trust = mcp
            .send_config_batch_write_request(ConfigBatchWriteParams {
                edits: vec![ConfigEdit {
                    key_path: "hooks.state".to_string(),
                    value: json!({hook.key.clone(): {"trusted_hash": hook.current_hash}}),
                    merge_strategy: MergeStrategy::Upsert,
                }],
                file_path: None,
                expected_version: None,
                reload_user_config: true,
            })
            .await?;
        let _: ConfigWriteResponse = mcp.read_response(trust).await?;
    }
    let request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = mcp.read_response(request).await?;
    let request = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(json!({
                "threadId": thread.id,
                "objective": "Finish <the> & update_plan work",
                "origin": "user",
            })),
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
        let expected = if recovery == Some("interrupt") && turn > 1 {
            TurnStatus::Interrupted
        } else {
            TurnStatus::Completed
        };
        assert_eq!(expected, completed.turn.status);
        assert_eq!(None, completed.turn.error);
    }
    timeout(Duration::from_secs(30), async {
        loop {
            let notification: ThreadGoalUpdatedNotification =
                mcp.read_notification("thread/goal/updated").await?;
            if notification.goal.status == ThreadGoalStatus::Blocked {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await??;
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
    let sampled = match recovery {
        // Blocked pre-turn compaction interrupts every later turn before it samples.
        Some("interrupt") => 1,
        Some("tool") => turns + 1,
        _ => turns,
    };
    assert_eq!(requests.len(), sampled);
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
        assert!(
            reminders
                .iter()
                .all(|text| !text.contains("Completion audit:"))
        );
        if index == 0 {
            // The user's recorded instruction states the objective, so no reminder repeats it.
            assert!(reminders.is_empty());
            assert!(texts.iter().any(|text| {
                text.contains(r#"User set the goal: "Finish <the> & update_plan work""#)
            }));
            continue;
        }
        let latest = reminders.last().expect("goal reminder");
        // The reminder carries no objective; the canonical goal message owns it.
        assert!(!latest.contains("<objective>"));
        assert!(!latest.contains("Finish <the> & update_plan work"));
        assert!(latest.contains(
            "The full goal-continuation instructions earlier in this context still apply."
        ));
        assert!(!latest.contains("Tokens used: 0\n"));
    }
    Ok(())
    })
    .await
}
