use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_features::Feature;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WorkStatus;
use codex_protocol::user_input::UserInput;
use codex_rollout::RolloutRecorder;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn work_status_updates_atomically_without_entering_history() -> Result<()> {
    let server = responses::start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::CodeModeOnly).unwrap();
    });
    let test = builder.build_with_auto_env(&server).await?;
    let updates = [
        json!({"activity": "Orphan"}),
        json!({"task": "Implement status", "activity": "Read renderer"}),
        json!({"activity": "Write tests"}),
        json!({"task": "Release", "activity": " "}),
        json!({}),
        json!({"task": "Release"}),
        json!({"activity": "x".repeat(161)}),
    ];
    let mut requests = Vec::new();
    for (index, update) in updates.iter().enumerate() {
        let id = format!("update-{index}");
        requests.push(
            responses::mount_sse_once(
                &server,
                responses::sse(vec![
                    responses::ev_function_call(&id, "update_work_status", &update.to_string()),
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
        request.body_json()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "update_work_status")
            || request
                .tool_by_name("functions", "update_work_status")
                .is_some(),
        "status must remain directly visible to the model in Code Mode Only"
    );
    let turn_id = snapshots
        .first()
        .expect("work status was emitted")
        .turn_id
        .clone();
    assert_eq!(
        snapshots,
        vec![
            WorkStatus {
                turn_id: turn_id.clone(),
                task: "Implement status".into(),
                activity: Some("Read renderer".into())
            },
            WorkStatus {
                turn_id: turn_id.clone(),
                task: "Implement status".into(),
                activity: Some("Write tests".into())
            },
            WorkStatus {
                turn_id,
                task: "Release".into(),
                activity: None
            },
        ]
    );
    for (index, expected) in [
        "Set a task before setting an activity.",
        "Work status updated.",
        "Work status updated.",
        "Summaries must contain visible text without control characters.",
        "Supply task, activity, or both.",
        "Work status updated.",
        "Keep each summary within 160 characters.",
    ]
    .iter()
    .enumerate()
    {
        let output = requests[index + 1]
            .single_request()
            .function_call_output_content_and_success(&format!("update-{index}"))
            .and_then(|(text, _)| text)
            .expect("tool reply");
        assert_eq!(output, *expected);
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
