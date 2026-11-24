//! The production server never installs the optional board reviewer.
use super::super::mcp_tool::wait_for_mcp_tool_call_completed;
use super::sync_sessions::sync_config;
use super::*;
use codex_app_server_protocol::GuardianApprovalReviewStatus;
use codex_app_server_protocol::ItemGuardianApprovalReviewCompletedNotification;
use pretty_assertions::assert_eq;
use test_case::test_case;

#[test_case(false, false; "native_without_v3")]
#[test_case(true, false; "native_with_legacy_v3_enabled")]
#[test_case(true, true; "ephemeral_uses_native_with_legacy_v3_enabled")]
#[tokio::test]
async fn pending_mcp_action_keeps_native_review(legacy_v3: bool, ephemeral: bool) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let responses = vec![
        responses::sse(vec![
            responses::ev_function_call_with_namespace(
                "reviewed-call",
                &format!("mcp__{TEST_SERVER_NAME}"),
                TEST_TOOL_NAME,
                r#"{"message":"check the release"}"#,
            ),
            responses::ev_completed("tool-call"),
        ]),
        responses::sse(vec![
            responses::ev_assistant_message(
                "verdict",
                r#"{"outcome":"allow","rationale":"The user authorized this check."}"#,
            ),
            responses::ev_completed("verdict"),
        ]),
        responses::sse(vec![
            responses::ev_assistant_message("done", "Done."),
            responses::ev_completed("done"),
        ]),
    ];
    let requests = responses::mount_sse_sequence(&server, responses).await;
    let (mcp_url, mcp_server) = start_mcp_server(/*sensitive_action*/ None).await?;
    let home = TempDir::new()?;
    sync_config(&server.uri())
        .with_extra_config(&format!("[features.guardianv3]\nenabled = {legacy_v3}"))
        .with_extra_config(&format!(
            "[mcp_servers.{TEST_SERVER_NAME}]\nurl = \"{mcp_url}/mcp\"\ndefault_tools_approval_mode = \"prompt\""
        ))
        .write(home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized_with_timeout(TIMEOUT)
        .await?;
    let thread = app
        .start_thread(ThreadStartParams {
            ephemeral: ephemeral.then_some(true),
            ..Default::default()
        })
        .await?
        .thread;
    let turn = app
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "Run the release check.".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let completed: ItemGuardianApprovalReviewCompletedNotification = timeout(
        TIMEOUT,
        app.read_notification("item/autoApprovalReview/completed"),
    )
    .await??;
    assert_eq!(turn.turn.status, TurnStatus::Completed);
    assert_eq!(
        (&completed.thread_id, &completed.turn_id),
        (&thread.id, &turn.turn.id)
    );
    assert_eq!(
        completed.review.status,
        GuardianApprovalReviewStatus::Approved
    );
    let completed_tool = wait_for_mcp_tool_call_completed(&mut app, "reviewed-call").await?;
    assert_eq!(
        (&completed_tool.thread_id, &completed_tool.turn_id),
        (&thread.id, &turn.turn.id)
    );
    assert!(matches!(
        completed_tool.item.payload,
        ThreadItemPayload::McpToolCall {
            status: McpToolCallStatus::Completed,
            ..
        }
    ));
    app.shutdown_gracefully().await?;

    let calls = requests.requests();
    let [_, verdict, actor] = calls.as_slice() else {
        anyhow::bail!("expected just the initial action, live verdict, and resumed actor");
    };
    assert_eq!(
        verdict.header("x-openai-subagent").as_deref(),
        Some("guardian")
    );
    assert!(verdict.body_contains_text("check the release"));
    assert!(actor.function_call_output_text("reviewed-call").is_some());
    assert!(
        !verdict.body_json()["tools"]
            .to_string()
            .contains("guardian_board")
    );
    mcp_server.abort();
    Ok(())
}
