use super::code_mode::custom_tool_output_last_non_empty_text;
use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_features::Feature;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SubAgentActivityKind;
use codex_protocol::protocol::WorkStatus;
use codex_protocol::user_input::UserInput;
use codex_rollout::RolloutRecorder;
use core_test_support::mock_server;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn work_status_replaces_activity_without_entering_history() -> Result<()> {
    let server = responses::start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::CodeModeOnly).unwrap();
    });
    let test = builder.build_with_auto_env(&server).await?;
    let updates = [
        json!({"activity": "  Read\n renderer "}),
        json!({"activity": "Write tests"}),
        json!({"activity": " "}),
        json!({"activity": "🦀".repeat(50)}),
        json!({"activity": "🦀".repeat(51)}),
        json!({"activity": 42}),
    ];
    let mut requests = Vec::new();
    for (index, update) in updates.iter().enumerate() {
        let id = format!("update-{index}");
        requests.push(
            responses::mount_sse_once(
                &server,
                responses::sse(vec![
                    responses::ev_custom_tool_call(
                        &id,
                        "exec",
                        &format!(
                            "text([await tools.update_work_status({update}), \"continued\"]);"
                        ),
                    ),
                    responses::ev_completed(&id),
                ]),
            )
            .await,
        );
    }
    requests.push(
        responses::mount_sse_once(
            &server,
            responses::sse(vec![
                responses::ev_assistant_message("answer", "Finished."),
                responses::ev_completed("finished"),
            ]),
        )
        .await,
    );
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Implement and release the status display.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let mut snapshots = Vec::new();
    wait_for_event(&test.codex, |event| {
        if let EventMsg::WorkStatusUpdated(status) = event {
            snapshots.push(status.clone());
        }
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let request = requests[0].single_request();
    assert!(
        request
            .tool_by_name("functions", "update_work_status")
            .is_none()
    );
    let turn_id = snapshots
        .first()
        .expect("work status was emitted")
        .turn_id
        .clone();
    for status in &mut snapshots {
        assert!(
            status
                .emitted_at
                .take()
                .is_some_and(|timestamp| timestamp > 0)
        );
    }
    assert_eq!(
        snapshots,
        vec![
            WorkStatus {
                turn_id: turn_id.clone(),
                activity: "Read renderer".into(),
                emitted_at: None,
            },
            WorkStatus {
                turn_id: turn_id.clone(),
                activity: "Write tests".into(),
                emitted_at: None,
            },
            WorkStatus {
                turn_id: turn_id.clone(),
                activity: "🦀".repeat(50),
                emitted_at: None,
            },
            WorkStatus {
                turn_id,
                activity: "🦀".repeat(50),
                emitted_at: None,
            },
        ]
    );
    for (index, expected) in [
        "Work status updated.",
        "Work status updated.",
        "Summaries must contain visible text without control characters.",
        "Work status updated.",
        "Warning: work status truncated to 50 characters.",
        "Warning: work status ignored; failed to parse function arguments",
    ]
    .iter()
    .enumerate()
    {
        let output = custom_tool_output_last_non_empty_text(
            &requests[index + 1].single_request(),
            &format!("update-{index}"),
        )
        .expect("tool reply");
        assert!(output.contains(expected), "{output}");
        assert!(output.contains("continued"), "{output}");
    }
    // Exercise the real rollout policy: presentation updates are not durable history.
    test.codex.flush_rollout().await?;
    let (rollout, _, _) =
        RolloutRecorder::load_rollout_items(&test.codex.rollout_path().expect("rollout path"))
            .await?;
    assert!(!rollout.iter().any(|item| matches!(
        item,
        codex_rollout::RolloutItem::EventMsg(EventMsg::WorkStatusUpdated(_))
    )));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_batches_work_status_and_agent_delivery_after_full_history_fork() -> Result<()> {
    let server = responses::start_mock_server().await;
    Box::pin(mock_server::run(&[&server], async {
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::CodeModeOnly).unwrap();
        config.features.enable(Feature::MultiAgentV2).unwrap();
    });
    let test = builder.build_with_auto_env(&server).await?;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_custom_tool_call(
                "spawn-cell",
                "exec",
                r#"text(await tools.wrangler_agents__spawn_agent({task_name: "worker", plaintext_message: "Read the inherited request.", fork_turns: "all"}));"#,
            ),
            responses::ev_completed("spawn"),
        ]),
    )
    .await;
    let fork_requests = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![responses::ev_completed("fork-1")]),
            responses::sse(vec![responses::ev_completed("fork-2")]),
        ],
    )
    .await;
    test.submit_turn("Inherited request: inspect the batchable tools.")
        .await?;
    let child_id = test
        .thread_manager
        .list_thread_ids()
        .await
        .into_iter()
        .find(|id| *id != test.session_configured.thread_id)
        .expect("full-history fork child");
    let child = test.thread_manager.get_thread(child_id).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(child.agent_status().await, AgentStatus::Completed(_)) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let child_request = fork_requests
        .requests()
        .into_iter()
        .find(|request| !request.inputs_of_type("agent_message").is_empty())
        .expect("child model request");
    assert!(child_request.body_contains_text("Inherited request: inspect the batchable tools."));
    // Forks retain conversation, not the parent's in-flight exec or any unmatched tool output.
    assert!(child_request.inputs_of_type("custom_tool_call").is_empty());
    assert!(
        child_request
            .inputs_of_type("custom_tool_call_output")
            .is_empty()
    );
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_custom_tool_call(
                "batch-cell",
                "exec",
                r#"text(await tools.update_work_status({activity: "Deliver follow-up"})); text(await tools.wrangler_agents__followup_task({target: "worker", plaintext_message: "Batched follow-up delivered."}));"#,
            ),
            responses::ev_completed("batch"),
        ]),
    ).await;
    let delivered = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![responses::ev_completed("delivery-1")]),
            responses::sse(vec![responses::ev_completed("delivery-2")]),
        ],
    )
    .await;
    let TurnInputSubmission::Started { turn_id, .. } = test
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Batch work status with agent delivery.".into(),
            text_elements: Vec::new(),
        }]))
        .await?
    else {
        anyhow::bail!("expected a new batch turn");
    };
    let mut statuses = Vec::new();
    let mut nested_activity = Vec::new();
    wait_for_event(&test.codex, |event| {
        if let EventMsg::WorkStatusUpdated(status) = event {
            statuses.push((status.turn_id.clone(), status.activity.clone()));
        }
        if let EventMsg::CodeModeOwned(owned) = event
            && let EventMsg::ItemCompleted(completed) = owned.payload().as_ref()
            && let TurnItem::SubAgentActivity(activity) = &completed.item
        {
            nested_activity.push((activity.agent_thread_id, activity.kind));
        }
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(statuses, vec![(turn_id, "Deliver follow-up".to_owned())]);
    assert_eq!(
        nested_activity,
        vec![(child_id, SubAgentActivityKind::Interacted)]
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(request) = delivered.requests().into_iter().find(|request| {
                request
                    .inputs_of_type("agent_message")
                    .iter()
                    .any(|item| item.to_string().contains("Batched follow-up delivered."))
            }) {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
    }))
    .await
}
