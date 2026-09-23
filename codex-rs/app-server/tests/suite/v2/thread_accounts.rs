use super::*;
use codex_app_server_protocol::ThreadAccountListResponse;
use codex_app_server_protocol::ThreadAccountSelectResponse;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use pretty_assertions::assert_eq;

// Exercise the public boundary, not just credential storage: the payer follows
// the conversation through selection and forks, never the last global login.
#[tokio::test]
async fn conversation_accounts_survive_selection_login_and_fork() -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    create_config_toml(
        home.path(),
        CreateConfigTomlParams {
            base_url: Some(server.uri()),
            chatgpt_base_url: Some(server.uri()),
            requires_openai_auth: Some(true),
            ..Default::default()
        },
    )?;
    for id in ["bob", "alice"] {
        write_chatgpt_auth(
            home.path(),
            ChatGptAuthFixture::new(format!("access-{id}"))
                .email(format!("{id}@example.com"))
                .plan_type("pro")
                .account_id(id)
                .chatgpt_account_id(id)
                .chatgpt_user_id(id),
            AuthCredentialsStoreMode::File,
        )?;
    }
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let mut threads = Vec::new();
    for _ in 0..2 {
        let request = app
            .send_thread_start_request_with_auto_env(ThreadStartParams {
                config: Some([("features.apps".into(), json!(true))].into()),
                ..Default::default()
            })
            .await?;
        let response: ThreadStartResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        threads.push(response.thread.id);
    }
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": threads[0], "accountId": "bob:bob"})),
        )
        .await?;
    let selected: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        selected,
        ThreadAccountSelectResponse {
            account_id: "bob:bob".into(),
            pending: false,
            default_saved: true
        }
    );
    let request = app
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let response: ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    threads.push(response.thread.id);

    // Logging in again changes the global active entry, not any pinned selector.
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("access-alice-new")
            .email("alice@example.com")
            .plan_type("pro")
            .account_id("alice")
            .chatgpt_account_id("alice")
            .chatgpt_user_id("alice"),
        AuthCredentialsStoreMode::File,
    )?;
    Mock::given(method("GET"))
        .and(path("/api/codex/accounts/check"))
        .respond_with(|request: &wiremock::Request| {
            let account_id = request.headers["chatgpt-account-id"]
                .to_str()
                .expect("workspace account header");
            ResponseTemplate::new(200).set_body_json(json!({"accounts": [{
                "id": account_id,
                "workspace_backend_origin": "https://chatgpt.com",
                "account_routing_override": "NO_CONSTRAINT"
            }]}))
        })
        .mount(&server)
        .await;
    for (thread, expected) in threads.iter().zip(["bob:bob", "alice:alice", "bob:bob"]) {
        let request = app
            .send_request("thread/account/list", Some(json!({"threadId": thread})))
            .await?;
        let response: ThreadAccountListResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(response.selected_account_id.as_deref(), Some(expected));
        assert_eq!(response.accounts.len(), 2);

        let request = app
            .send_get_account_request(GetAccountParams {
                thread_id: Some(thread.clone()),
                refresh_token: false,
            })
            .await?;
        let response: GetAccountResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        let id = expected.split_once(':').expect("account selector").0;
        assert_eq!(
            response,
            GetAccountResponse {
                account_id: Some(expected.to_string()),
                account: Some(Account::Chatgpt {
                    email: Some(format!("{id}@example.com")),
                    plan_type: AccountPlanType::Pro,
                }),
                requires_openai_auth: true,
                workspace_routing: expected_workspace_routing(id),
            }
        );
    }

    let mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("payer"),
            responses::ev_completed("payer"),
        ]),
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: threads[0].clone(),
            input: vec![UserInput::Text {
                text: "Hello".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let _ = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    assert_eq!(
        mock.single_request().header("authorization"),
        Some("Bearer access-bob".into())
    );
    assert_eq!(
        mock.single_request().header("chatgpt-account-id"),
        Some("bob".into())
    );

    // Metadata lookups must use the same identity as inference, not reopen the
    // singleton credential entry midway through the app request pipeline.
    Mock::given(method("POST"))
        .and(path("/ps/apps/batch"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer access-bob",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"apps": []})))
        .expect(1)
        .mount(&server)
        .await;
    let request = app
        .send_request(
            "app/read",
            Some(json!({"threadId": threads[0], "appIds": ["calendar"]})),
        )
        .await?;
    let response: codex_app_server_protocol::AppsReadResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(response.missing_app_ids, vec!["calendar"]);

    let initial = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("initial"),
            responses::ev_completed("initial"),
        ]),
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: threads[2].clone(),
            input: vec![UserInput::Text {
                text: "Persist the initial account".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let _ = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    assert_eq!(
        initial.single_request().header("authorization"),
        Some("Bearer access-bob".into())
    );

    let request = app
        .send_thread_fork_request(ThreadForkParams {
            thread_id: threads[0].clone(),
            ..Default::default()
        })
        .await?;
    let fork: ThreadForkResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": threads[0], "accountId": "alice:alice"})),
        )
        .await?;
    let _: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let request = app
        .send_request(
            "thread/account/list",
            Some(json!({"threadId": fork.thread.id})),
        )
        .await?;
    let response: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(response.selected_account_id.as_deref(), Some("bob:bob"));

    app.shutdown_gracefully().await?;
    app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    for (id, account) in [
        (&threads[0], "alice:alice"),
        (&fork.thread.id, "bob:bob"),
        (&threads[2], "bob:bob"),
    ] {
        let request = app
            .send_thread_resume_request(codex_app_server_protocol::ThreadResumeParams {
                thread_id: id.clone(),
                ..Default::default()
            })
            .await?;
        let _: codex_app_server_protocol::ThreadResumeResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        let request = app
            .send_request("thread/account/list", Some(json!({"threadId": id})))
            .await?;
        let response: ThreadAccountListResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(response.selected_account_id.as_deref(), Some(account));
    }

    // A live tool is a deterministic boundary: selection must not retarget it,
    // but its continuation must use the new payer without another user turn.
    let handoff = responses::mount_response_sequence(
        &server,
        vec![
            responses::sse_response(app_test_support::create_request_user_input_sse_response(
                "account-boundary",
            )?)
            .insert_header("x-codex-turn-state", "previous-payer-routing"),
            responses::sse_response(responses::sse(vec![responses::ev_completed("switched")])),
        ],
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: threads[0].clone(),
            input: vec![UserInput::Text {
                text: "Continue".into(),
                text_elements: Vec::new(),
            }],
            collaboration_mode: Some(codex_protocol::config_types::CollaborationMode {
                mode: codex_protocol::config_types::ModeKind::Plan,
                settings: codex_protocol::config_types::Settings {
                    model: "mock-model".into(),
                    reasoning_effort: None,
                    developer_instructions: None,
                },
            }),
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let tool = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_request_message(),
    )
    .await??;
    let codex_app_server_protocol::ServerRequest::ToolRequestUserInput {
        request_id: tool_id,
        ..
    } = tool
    else {
        anyhow::bail!("expected a user-input tool, got {tool:?}");
    };
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": threads[0], "accountId": "bob:bob"})),
        )
        .await?;
    let selected: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        selected,
        ThreadAccountSelectResponse {
            account_id: "bob:bob".into(),
            pending: true,
            default_saved: true
        },
    );
    let request = app
        .send_request("thread/account/list", Some(json!({"threadId": threads[0]})))
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("alice:alice"), Some("bob:bob"))
    );
    app.send_response(tool_id, json!({"answers": {}})).await?;
    let _ = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let requests = handoff.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].header("chatgpt-account-id"),
        Some("alice".into())
    );
    assert_eq!(requests[1].header("chatgpt-account-id"), Some("bob".into()));
    assert_eq!(
        requests[1].header("authorization"),
        Some("Bearer access-bob".into())
    );
    assert_eq!(requests[1].header("x-codex-turn-state"), None);
    assert!(
        requests[1]
            .function_call_output("account-boundary")
            .is_object()
    );
    let request = app
        .send_request("thread/account/list", Some(json!({"threadId": threads[0]})))
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("bob:bob"), None)
    );

    // Existing descendants roll independently even while their parent is blocked.
    let request = app
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            config: Some([("features.multi_agent_v2.enabled".into(), json!(true))].into()),
            approval_policy: Some(codex_app_server_protocol::AskForApproval::OnRequest),
            approvals_reviewer: Some(codex_app_server_protocol::ApprovalsReviewer::User),
            sandbox: Some(codex_app_server_protocol::SandboxMode::WorkspaceWrite),
            ..Default::default()
        })
        .await?;
    let family: ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    responses::mount_sse_once(&server, responses::sse(vec![
        responses::ev_function_call_with_namespace("spawn-family-worker", "wrangler_agents", "spawn_agent",
            &json!({"task_name": "worker", "message": "family-worker-only", "fork_turns": "none"}).to_string()),
        responses::ev_completed("family-spawn"),
    ])).await;
    let child = responses::mount_sse_once_match(&server, |request: &wiremock::Request| {
        let body = String::from_utf8_lossy(&request.body);
        body.contains("family-worker-only") && !body.contains("spawn-family-worker")
    }, responses::sse(vec![
        responses::ev_function_call("pause-family-worker", "exec_command",
            r#"{"cmd":"printf family-worker-done","sandbox_permissions":"require_escalated","justification":"account handoff approval boundary"}"#),
        responses::ev_completed("worker-approval"),
    ])).await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            String::from_utf8_lossy(&request.body).contains("spawn-family-worker")
        },
        app_test_support::create_request_user_input_sse_response("pause-family-root")?,
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: family.thread.id.clone(),
            input: vec![UserInput::Text {
                text: "Start the family".into(),
                text_elements: Vec::new(),
            }],
            collaboration_mode: Some(codex_protocol::config_types::CollaborationMode {
                mode: codex_protocol::config_types::ModeKind::Plan,
                settings: codex_protocol::config_types::Settings {
                    model: "mock-model".into(),
                    reasoning_effort: None,
                    developer_instructions: None,
                },
            }),
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let mut questions = std::collections::HashMap::new();
    for _ in 0..2 {
        let request = timeout(
            DEFAULT_READ_TIMEOUT,
            app.read_stream_until_request_message(),
        )
        .await??;
        let (thread_id, request_id) = match request {
            codex_app_server_protocol::ServerRequest::ToolRequestUserInput {
                request_id,
                params,
            } => (params.thread_id, request_id),
            codex_app_server_protocol::ServerRequest::CommandExecutionRequestApproval {
                request_id,
                params,
            } => (params.thread_id, request_id),
            request => anyhow::bail!("expected family input request, got {request:?}"),
        };
        questions.insert(thread_id, request_id);
    }
    let root_question = questions.remove(&family.thread.id).expect("root question");
    let (child_id, child_question) = questions.into_iter().next().expect("child question");
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": family.thread.id, "accountId": "alice:alice"})),
        )
        .await?;
    let selected: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert!(selected.pending);
    let child_continued = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("worker-switched")]),
    )
    .await;
    app.send_response(child_question, json!({"decision": "decline"}))
        .await?;
    let _ = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    assert_eq!(
        child.single_request().header("chatgpt-account-id"),
        Some("bob".into())
    );
    assert_eq!(
        child_continued
            .single_request()
            .header("chatgpt-account-id"),
        Some("alice".into())
    );
    let request = app
        .send_request("thread/account/list", Some(json!({"threadId": child_id})))
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("alice:alice"), None)
    );
    let request = app
        .send_request(
            "thread/account/list",
            Some(json!({"threadId": family.thread.id})),
        )
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("bob:bob"), Some("alice:alice"))
    );

    // Restart with the parent still waiting: its durable selection must not revert.
    app.shutdown_gracefully().await?;
    drop(root_question);
    app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let request = app
        .send_thread_resume_request(codex_app_server_protocol::ThreadResumeParams {
            thread_id: family.thread.id.clone(),
            ..Default::default()
        })
        .await?;
    let _: codex_app_server_protocol::ThreadResumeResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let request = app
        .send_request(
            "thread/account/list",
            Some(json!({"threadId": family.thread.id})),
        )
        .await?;
    let accounts: ThreadAccountListResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    assert_eq!(
        (
            accounts.selected_account_id.as_deref(),
            accounts.pending_account_id.as_deref()
        ),
        (Some("alice:alice"), None)
    );
    // Retry backoff is a request boundary too: never require an overloaded
    // account to recover before the requested replacement can be used.
    let retried = responses::mount_response_sequence(
        &server,
        vec![
            ResponseTemplate::new(503)
                .set_body_json(json!({"error": {"code": "server_is_overloaded"}})),
            responses::sse_response(responses::sse(vec![responses::ev_completed(
                "new-account-retry",
            )])),
        ],
    )
    .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: family.thread.id.clone(),
            input: vec![UserInput::Text {
                text: "Continue after capacity".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let retry: codex_app_server_protocol::ErrorNotification =
        timeout(DEFAULT_READ_TIMEOUT, app.read_notification("error")).await??;
    assert!(retry.will_retry);
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": family.thread.id, "accountId": "bob:bob"})),
        )
        .await?;
    let _: ThreadAccountSelectResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let _ = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    assert_eq!(
        retried
            .requests()
            .iter()
            .map(|request| request.header("chatgpt-account-id"))
            .collect::<Vec<_>>(),
        vec![Some("alice".into()), Some("bob".into())]
    );
    Ok(())
}
