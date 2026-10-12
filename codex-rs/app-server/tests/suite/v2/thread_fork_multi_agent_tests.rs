//! Fork snapshots preserve the source runtime and freeze inherited partial turns.

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_command_execution_sse_response;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::write_models_cache_with_models;
use codex_app_server_protocol::ApprovalsReviewer;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::CommandExecutionRequestApprovalResponse;
use codex_app_server_protocol::SandboxMode;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_protocol::ThreadUnsubscribeParams;
use codex_app_server_protocol::ThreadUnsubscribeResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use codex_protocol::protocol::MultiAgentVersion;
use codex_rollout::read_session_meta_line;
use core_test_support::load_default_config_for_test;
use core_test_support::mock_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::timeout;

const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[test_case::test_case(ThreadHistoryMode::Legacy; "legacy")]
#[test_case::test_case(ThreadHistoryMode::Paginated; "paginated")]
#[tokio::test]
async fn running_thread_fork_keeps_parent_approval(history_mode: ThreadHistoryMode) -> Result<()> {
    // Holding approval pins an actually running turn with a persisted, dangling tool call.
    // A delayed final model response alone cannot expose accidental tool execution in the fork.
    let server = create_mock_responses_server_sequence(vec![
        create_command_execution_sse_response(
            vec![
                "python3".into(),
                "-c".into(),
                "print('parent-finished')".into(),
            ],
            /*workdir*/ None,
            /*timeout_ms*/ None,
            "parent-call",
        )?,
        create_final_assistant_message_sse_response("Fork done")?,
        create_final_assistant_message_sse_response("Parent done")?,
    ])
    .await;
    mock_server::run(&[&server], async {
        let codex_home = TempDir::new()?;
        MockResponsesConfig::new(&server.uri())
            .enable_feature(Feature::MultiAgentV2)
            .with_root_config(r#"approvals_reviewer = "user""#)
            .write(codex_home.path())?;
        let mut mcp = TestAppServer::builder()
            .with_codex_home(codex_home.path())
            .build_initialized()
            .await?;
        let parent = mcp
            .start_thread(ThreadStartParams {
                history_mode: Some(history_mode),
                ..Default::default()
            })
            .await?
            .thread;
        let TurnStartResponse { turn: parent_turn } = mcp
            .request(|request_id| ClientRequest::TurnStart {
                request_id,
                params: TurnStartParams {
                    thread_id: parent.id.clone(),
                    approval_policy: Some(AskForApproval::UnlessTrusted),
                    input: vec![UserInput::Text {
                        text: "Run the command".into(),
                        text_elements: Vec::new(),
                    }],
                    ..Default::default()
                },
            })
            .await?;
        let approval = timeout(READ_TIMEOUT, mcp.read_stream_until_request_message()).await??;
        let ServerRequest::CommandExecutionRequestApproval {
            request_id: approval_id,
            params,
        } = approval
        else {
            panic!("expected command approval");
        };
        assert_eq!(
            (&params.thread_id, &params.turn_id, params.item_id.as_str()),
            (&parent.id, &parent_turn.id, "parent-call")
        );
        let ThreadForkResponse { thread: fork, .. } = mcp
            .request(|request_id| ClientRequest::ThreadFork {
                request_id,
                params: ThreadForkParams {
                    thread_id: parent.id.clone(),
                    defer_goal_continuation: true,
                    ..Default::default()
                },
            })
            .await?;
        assert_eq!(fork.status, ThreadStatus::Idle);
        assert_eq!(
            fork.turns.last().expect("partial inherited turn").status,
            TurnStatus::Interrupted
        );
        let _: ThreadUnsubscribeResponse = mcp
            .request(|request_id| ClientRequest::ThreadUnsubscribe {
                request_id,
                params: ThreadUnsubscribeParams {
                    thread_id: parent.id.clone(),
                },
            })
            .await?;

        let completed = mcp
            .start_turn_and_wait_for_completion(TurnStartParams {
                thread_id: fork.id.clone(),
                input: vec![UserInput::Text {
                    text: "Continue in the fork".into(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            })
            .await?;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
        let requests = server.received_requests().await.expect("model requests");
        let input = requests
            .last()
            .expect("fork model request")
            .body_json::<Value>()?["input"]
            .clone();
        assert!(serde_json::to_string(&input)?.contains("<turn_aborted>"));
        assert!(
            input
                .as_array()
                .expect("model input")
                .iter()
                .any(|item| item["type"] == "function_call_output"
                    && item["call_id"] == "parent-call"
                    && item["output"] == "aborted")
        );

        // Navigation unsubscribes the parent, but does not cancel its turn or resolve approval.
        let resumed: ThreadResumeResponse = mcp
            .request(|request_id| ClientRequest::ThreadResume {
                request_id,
                params: ThreadResumeParams {
                    thread_id: parent.id.clone(),
                    ..Default::default()
                },
            })
            .await?;
        assert!(matches!(resumed.thread.status, ThreadStatus::Active { .. }));
        assert_eq!(
            resumed
                .thread
                .turns
                .last()
                .expect("running parent turn")
                .status,
            TurnStatus::InProgress
        );
        let replayed = timeout(READ_TIMEOUT, mcp.read_stream_until_request_message()).await??;
        let ServerRequest::CommandExecutionRequestApproval { request_id, params } = replayed else {
            panic!("expected replayed command approval");
        };
        assert_eq!(request_id, approval_id);
        assert_eq!(
            (&params.thread_id, &params.turn_id),
            (&parent.id, &parent_turn.id)
        );
        mcp.send_response(
            request_id,
            serde_json::to_value(CommandExecutionRequestApprovalResponse {
                decision: CommandExecutionApprovalDecision::Accept,
            })?,
        )
        .await?;
        let completed: TurnCompletedNotification =
            timeout(READ_TIMEOUT, mcp.read_notification("turn/completed")).await??;
        assert_eq!(
            (
                completed.thread_id,
                completed.turn.id,
                completed.turn.status
            ),
            (parent.id, parent_turn.id, TurnStatus::Completed)
        );
        let requests = server.received_requests().await.expect("model requests");
        let input = requests
            .last()
            .expect("parent model request")
            .body_json::<Value>()?["input"]
            .clone();
        assert!(!serde_json::to_string(&input)?.contains("<turn_aborted>"));
        assert!(input.as_array().expect("model input").iter().any(|item| {
            item["type"] == "function_call_output"
                && item["call_id"] == "parent-call"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("parent-finished"))
        }));
        Ok(())
    })
    .await
}

enum SourceState {
    Loaded,
    Unloaded,
}

#[test_case::test_case(ThreadHistoryMode::Legacy, SourceState::Loaded; "legacy_loaded")]
#[test_case::test_case(ThreadHistoryMode::Legacy, SourceState::Unloaded; "legacy_unloaded")]
#[test_case::test_case(ThreadHistoryMode::Paginated, SourceState::Loaded; "paginated_loaded")]
#[test_case::test_case(ThreadHistoryMode::Paginated, SourceState::Unloaded; "paginated_unloaded")]
#[tokio::test]
async fn fork_before_first_turn_preserves_model_selected_multi_agent_version(
    history_mode: ThreadHistoryMode,
    source_state: SourceState,
) -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .disable_feature(Feature::MultiAgentV2)
        .write(codex_home.path())?;
    let config = load_default_config_for_test(&codex_home).await;
    let mut model = codex_core::test_support::construct_model_info_offline("mock-model", &config);
    model.multi_agent_version = Some(MultiAgentVersion::V2);
    write_models_cache_with_models(codex_home.path(), vec![model]).await?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let parent = mcp
        .start_thread(ThreadStartParams {
            history_mode: Some(history_mode),
            ..Default::default()
        })
        .await?
        .thread;
    let completed = mcp
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: parent.id.clone(),
            input: vec![UserInput::Text {
                text: "First message".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    if matches!(source_state, SourceState::Unloaded) {
        mcp.shutdown_gracefully().await?;
        mcp = TestAppServer::builder()
            .with_codex_home(codex_home.path())
            .build_initialized()
            .await?;
    }
    let ThreadForkResponse { thread: child, .. } = mcp
        .request(|request_id| ClientRequest::ThreadFork {
            request_id,
            params: ThreadForkParams {
                thread_id: parent.id,
                before_turn_id: Some(completed.turn.id),
                exclude_turns: true,
                // Version recovery must also work when no permission settings need restoring.
                approval_policy: Some(AskForApproval::Never),
                approvals_reviewer: Some(ApprovalsReviewer::User),
                sandbox: Some(SandboxMode::ReadOnly),
                ..Default::default()
            },
        })
        .await?;
    assert_eq!(
        read_session_meta_line(child.path.as_ref().expect("fork rollout").as_path())
            .await?
            .meta
            .multi_agent_version,
        Some(MultiAgentVersion::V2)
    );

    // Resume before the child's first turn can persist another version-bearing context.
    mcp.shutdown_gracefully().await?;
    mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let _: ThreadResumeResponse = mcp
        .request(|request_id| ClientRequest::ThreadResume {
            request_id,
            params: ThreadResumeParams {
                thread_id: child.id.clone(),
                exclude_turns: true,
                ..Default::default()
            },
        })
        .await?;
    mcp.start_turn_and_wait_for_completion(TurnStartParams {
        thread_id: child.id,
        input: vec![UserInput::Text {
            text: "Forked message".to_string(),
            text_elements: Vec::new(),
        }],
        ..Default::default()
    })
    .await?;

    let requests = server.received_requests().await.expect("response requests");
    let mut multi_agent_namespaces = Vec::new();
    for request in requests
        .iter()
        .filter(|request| request.url.path().ends_with("/responses"))
    {
        let body = request.body_json::<Value>()?;
        multi_agent_namespaces.push(
            body["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .filter(|name| matches!(*name, "wrangler_agents" | "multi_agent_v1"))
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        multi_agent_namespaces,
        vec![vec!["wrangler_agents"], vec!["wrangler_agents"]]
    );
    Ok(())
}
