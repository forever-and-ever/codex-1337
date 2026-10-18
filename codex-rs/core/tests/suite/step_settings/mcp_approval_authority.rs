//! MCP approvals answer to the issuing turn, not thread settings saved for later turns.

use super::*;
use core_test_support::mock_server;
use core_test_support::responses::ev_assistant_message;
use core_test_support::submit_thread_settings;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_write_approval_follows_the_issuing_turn_not_saved_thread_settings() -> Result<()> {
    skip_if_no_network!(Ok(()));
    core_test_support::skip_if_wine_exec!(Ok(()), "requires a Windows test_stdio_server binary");

    let server = start_mock_server().await;
    Box::pin(mock_server::run(&[&server], async {
        let responses = mount_sse_sequence(
            &server,
            vec![
                paused_response("pause-before-thread-update", "pause"),
                sse(vec![
                    ev_response_created("resp-write"),
                    ev_function_call_with_namespace(
                        "write",
                        "mcp__rmcp",
                        "get_daybreak_access",
                        "{}",
                    ),
                    ev_completed("resp-write"),
                ]),
                sse(vec![
                    ev_response_created("resp-guardian"),
                    ev_assistant_message(
                        "msg-guardian",
                        &json!({
                            "risk_level": "low",
                            "user_authorization": "high",
                            "outcome": "allow",
                            "rationale": "Reading account access is low risk.",
                        })
                        .to_string(),
                    ),
                    ev_completed("resp-guardian"),
                ]),
                sse_completed("resp-done"),
            ],
        )
        .await;
        let command = remote_aware_stdio_server_bin()?;
        let test = step_settings_test()
            .with_config(move |config| {
                config.permissions.approval_policy = Constrained::allow_any(AskForApproval::Never);
                config
                    .permissions
                    .set_permission_profile(PermissionProfile::workspace_write())
                    .expect("set managed workspace-write permissions");
                config
                    .features
                    .enable(Feature::GuardianApproval)
                    .expect("enable automatic review");
                config
                    .mcp_servers
                    .set(HashMap::from([(
                        "rmcp".to_string(),
                        serde_json::from_value(json!({
                            "command": command,
                            "env": { "MCP_TEST_DAYBREAK_READ_ONLY": "false" },
                            "environment_id": remote_aware_environment_id(),
                            "cwd": config.cwd,
                        }))
                        .expect("test MCP config"),
                    )]))
                    .expect("configure MCP");
            })
            .build_with_auto_env(&server)
            .await?;
        wait_for_mcp_server(&test.codex, "rmcp").await?;
        submit_thread_settings(
            &test.codex,
            ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::OnRequest),
                approvals_reviewer: Some(ApprovalsReviewer::AutoReview),
                permission_profile: Some(PermissionProfile::workspace_write()),
                ..Default::default()
            },
        )
        .await?;
        let paused = start_paused_turn(&test.codex).await?;
        // Saved for later turns: the running turn keeps on-request automatic review.
        submit_thread_settings(
            &test.codex,
            ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::Never),
                ..Default::default()
            },
        )
        .await?;
        answer_paused_turn(&test.codex, &paused.turn_id).await?;
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;

        let requests = responses.requests();
        let output = requests
            .last()
            .expect("final model request")
            .function_call_output("write")["output"]
            .to_string();
        assert!(!output.contains("requires approval"), "{output}");
        assert_eq!(
            requests
                .iter()
                .map(|request| {
                    request.body_json()["client_metadata"]["x-openai-subagent"]
                        .as_str()
                        .map(str::to_string)
                })
                .collect::<Vec<_>>(),
            [None, None, Some("guardian".to_string()), None],
        );
        Ok(())
    }))
    .await
}
