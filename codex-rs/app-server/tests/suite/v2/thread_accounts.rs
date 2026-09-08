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
    for (thread, expected) in threads.iter().zip(["bob:bob", "alice:alice", "bob:bob"]) {
        let request = app
            .send_request("thread/account/list", Some(json!({"threadId": thread})))
            .await?;
        let response: ThreadAccountListResponse =
            timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
        assert_eq!(response.selected_account_id.as_deref(), Some(expected));
        assert_eq!(response.accounts.len(), 2);
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

    // Keep inference pending until interruption; selection must not move its payer.
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;
    let request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: threads[0].clone(),
            input: vec![UserInput::Text {
                text: "Continue".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let _: TurnStartResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(request)).await??;
    let _ = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/started"),
    )
    .await??;
    let request = app
        .send_request(
            "thread/account/select",
            Some(json!({"threadId": threads[0], "accountId": "bob:bob"})),
        )
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_error_message(RequestId::Integer(request)),
    )
    .await??;
    assert_eq!(
        error.error.message,
        "Stop the current turn before changing accounts."
    );
    Ok(())
}
