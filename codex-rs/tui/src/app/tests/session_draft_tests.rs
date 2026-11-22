use super::session_lifecycle_requests::recorded_params;
use super::session_lifecycle_requests::start_recording_remote_app_server;
use super::*;
use crate::chatwidget::SessionDraft;
use crate::status::StatusAccountDisplay;
use clap::Parser;
use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::AccountUpdatedNotification;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::ModelProviderOption;
use codex_app_server_protocol::ServerNotification;
use codex_protocol::account::PlanType;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn session_draft_switches_provider_without_creating_threads_or_losing_input() -> Result<()> {
    let (mut app, mut events, _ops) = Box::pin(make_test_app_with_channels()).await;
    let http = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"accounts": [{"id": "test-account", "plan_type": "pro", "workspace_backend_origin": "https://chatgpt.com", "account_routing_override": "NO_CONSTRAINT"}]}),
        ))
        .mount(&http)
        .await;
    app.config.chatgpt_base_url = format!("{}/backend-api", http.uri());
    app.config.cli_auth_credentials_store_mode = codex_login::AuthCredentialsStoreMode::File;
    std::fs::write(
        app.config.codex_home.join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = {:?}\n",
            app.config.chatgpt_base_url
        ),
    )?;
    app_test_support::write_chatgpt_auth(
        &app.config.codex_home,
        app_test_support::ChatGptAuthFixture::new("test-access-token")
            .account_id("test-account")
            .chatgpt_account_id("test-account")
            .chatgpt_user_id("test-user")
            .plan_type("pro"),
        codex_login::AuthCredentialsStoreMode::File,
    )
    .expect("test account fixture");
    let (mut server, requests, proxy) =
        Box::pin(start_recording_remote_app_server(&app.config)).await?;
    let mut tui = tui::test_support::make_test_tui()?;
    app.chat_widget.session_draft = Some(SessionDraft::Editing);
    // Routing discovery can publish the global login after draft bootstrap has
    // already resolved a different account for the new conversation.
    let selected_account = Some(StatusAccountDisplay::ChatGpt {
        email: Some("main@example.com".into()),
        plan: Some("Pro".into()),
        workspace: None,
    });
    app.chat_widget
        .set_status_account_display(selected_account.clone());
    app.handle_app_server_event(
        &server,
        AppServerEvent::ServerNotification(Box::new(ServerNotification::AccountUpdated(
            AccountUpdatedNotification {
                reason: None,
                auth_mode: Some(AuthMode::Chatgpt),
                plan_type: Some(PlanType::Plus),
            },
        ))),
    )
    .await;
    assert_eq!(
        app.chat_widget.status_account_display(),
        selected_account.as_ref()
    );
    // Usage resolves the displayed fresh-root account before checking access; it must
    // neither trust stale signed-out flags nor create a conversation to discover identity.
    for command in ["/usage", "/usage daily"] {
        if command == "/usage daily" {
            // A payer refresh cannot grant or revoke the connection's reported authority.
            assert!(app.chat_widget.has_codex_backend_auth());
            app.chat_widget
                .update_account_state(crate::app_server_session::SessionAccount {
                    requires_openai_auth: app.chat_widget.requires_openai_auth(),
                    ..Default::default()
                });
        }
        while events.try_recv().is_ok() {}
        app.chat_widget.insert_str(command);
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let event = std::iter::from_fn(|| events.try_recv().ok())
            .find(|event| matches!(event, AppEvent::Usage { .. }))
            .expect("draft usage request");
        Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
        if command == "/usage" {
            // A concurrent identity refresh may supersede the read, not the user's command.
            app.read_displayed_account(&server, None);
        }
        app.settle_server_ui_requests(&mut tui, &mut server).await?;
        assert!(
            app.chat_widget.has_codex_backend_auth(),
            "draft account: {:?}",
            app.account_ui.displayed
        );
        assert_eq!(app.chat_widget.session_draft, Some(SessionDraft::Editing));
        assert_eq!(app.chat_widget.thread_id(), None);
        let replies = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            !replies
                .iter()
                .any(|event| matches!(event, AppEvent::StartDraftSession))
        );
        if command == "/usage daily" {
            assert!(replies.iter().any(|event| matches!(
                event,
                AppEvent::OpenAnalytics {
                    view: Some(crate::analytics::TokenActivityView::Daily)
                }
            )));
        }
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    }
    let account_reads = recorded_params(&requests, "account/read");
    assert_eq!(account_reads.len(), 3);
    assert!(account_reads.iter().all(|params| {
        params
            .get("threadId")
            .is_none_or(serde_json::Value::is_null)
            && params
                .get("newThread")
                .is_some_and(serde_json::Value::is_object)
    }));
    assert_eq!(
        recorded_params(&requests, "thread/start"),
        Vec::<serde_json::Value>::new()
    );
    // A deferred command leaves exactly its own draft for startup to replay.
    for draft in ["/account", "/rename Fix login"] {
        app.chat_widget.insert_str(draft);
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.chat_widget.session_draft, Some(SessionDraft::Starting));
        app.chat_widget.restore_session_draft();
        assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    }
    // Sign-in belongs to the provider, not to a thread that could not be created.
    app.chat_widget.session_draft = Some(SessionDraft::AwaitingSignIn(
        codex_app_server_protocol::AccountProvider::ClaudeSubscription,
    ));
    while events.try_recv().is_ok() {}
    app.chat_widget.insert_str("/account");
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        app.chat_widget.session_draft,
        Some(SessionDraft::AwaitingSignIn(
            codex_app_server_protocol::AccountProvider::ClaudeSubscription
        ))
    );
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::AddClaudeAccount))
    );
    assert_eq!(
        recorded_params(&requests, "thread/start"),
        Vec::<serde_json::Value>::new()
    );
    app.chat_widget.restore_session_draft();
    let original_model = app
        .model_catalog
        .models
        .iter()
        .find(|model| model.model == app.chat_widget.current_model())
        .expect("current model")
        .clone();
    let original_provider = ModelProviderOption {
        id: app.config.model_provider_id.clone(),
        name: app.config.model_provider.name.clone(),
    };
    app.chat_widget.insert_str("Keep this draft");
    app.chat_widget.attach_image(
        app.config.cwd.join("draft-image.png").to_path_buf(),
        crate::chatwidget::ClipboardPasteContext::Composer,
    );
    let original_input = serde_json::to_value(app.chat_widget.capture_thread_input_state())?
        .get("composer")
        .expect("serialized composer")
        .clone();
    let mut claude = original_model.clone();
    claude.model = "claude-sonnet-5".into();
    claude.display_name = "Sonnet 5".into();
    for (provider, model) in [
        (
            ModelProviderOption {
                id: "claude_subscription".into(),
                name: "Claude subscription".into(),
            },
            claude,
        ),
        (original_provider, original_model),
    ] {
        app.handle_provider_model_event(
            &mut tui,
            &mut server,
            ProviderModelEvent::Start(Box::new(ProviderModelChoice {
                effort: model.default_reasoning_effort.clone(),
                provider,
                model,
            })),
        )
        .await;
        app.settle_server_ui_requests(&mut tui, &mut server).await?;
        assert_eq!(
            serde_json::to_value(app.chat_widget.capture_thread_input_state())?["composer"],
            original_input
        );
        assert_eq!(app.chat_widget.thread_id(), None);
        assert_eq!(
            recorded_params(&requests, "thread/start"),
            Vec::<serde_json::Value>::new()
        );
        // Reload must pin this draft's provider even if another TUI changes the global default.
        let retained = crate::reload::RetainedInput {
            primary: None,
            overview: false,
            draft: None,
            unstarted: Some(crate::reload::RetainedSessionDraft {
                provider: app.chat_widget.config_ref().model_provider_id.clone(),
                model: app.chat_widget.current_model().to_owned(),
                cwd: app.config.cwd.clone(),
                input: app.chat_widget.capture_thread_input_state().unwrap(),
            }),
            kill_buffer: app.chat_widget.take_kill_buffer_snapshot(),
            terminal: None,
            cwd: Some(app.config.cwd.clone()),
            threads: HashMap::new(),
            detached: HashMap::new(),
        };
        let retained: crate::reload::RetainedInput =
            serde_json::from_slice(&serde_json::to_vec(&retained)?)?;
        let mut cli = crate::cli::Cli::try_parse_from(["codex", "--prompt", "must not replay"])?;
        retained.prepare_cli(&mut cli);
        assert_eq!(
            (
                cli.prompt.as_deref(),
                cli.model.as_deref(),
                cli.resume_session_id.as_deref(),
                cli.config_overrides.raw_overrides.as_slice()
            ),
            (
                None,
                Some(app.chat_widget.current_model()),
                None,
                [format!(
                    "model_provider={}",
                    toml::Value::String(app.chat_widget.config_ref().model_provider_id.clone())
                )]
                .as_slice(),
            )
        );
        assert_eq!(
            serde_json::to_value(retained.unstarted.unwrap().input)?["composer"],
            original_input
        );
    }
    assert!(
        app.chat_widget.has_codex_backend_auth(),
        "draft account: {:?}",
        app.account_ui.displayed
    );
    insta::assert_snapshot!(
        "session_draft_provider_switch",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );

    // Visiting another conversation must not discard an unstarted composer's input.
    let replacement = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui,
        app.config.clone(),
        /*initial_user_message*/ None,
    );
    app.replace_chat_widget(ChatWidget::new_with_app_event(replacement));
    assert!(app.agents_overview.parked_draft.is_some());
    assert!(
        app.restore_parked_session_draft(&mut tui, &mut server)
            .await?
    );
    Box::pin(app.settle_navigation(&mut tui, &mut server)).await?;
    assert_eq!(
        serde_json::to_value(app.chat_widget.capture_thread_input_state())?["composer"],
        original_input
    );
    assert_eq!(
        recorded_params(&requests, "thread/start"),
        Vec::<serde_json::Value>::new()
    );

    while events.try_recv().is_ok() {}
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.chat_widget.session_draft, Some(SessionDraft::Starting));
    assert_eq!(
        std::iter::from_fn(|| events.try_recv().ok())
            .filter(|event| matches!(event, AppEvent::StartDraftSession))
            .count(),
        1
    );
    // A rejected creation restores the submitted input, including its attachment, for retry.
    app.pending_startup_thread_start = true;
    app.handle_startup_thread_started(&mut server, Err(TypedRequestError::Server {
        method: "thread/start".into(),
        source: JSONRPCErrorError {
            code: -32600, message: "sign-in needed".into(),
            data: Some(serde_json::json!({"codexErrorInfo": {"signInRequired": {"provider": "claudeSubscription", "account": "claude@example.test"}}})),
        },
    }.into()))
        .await?;
    assert_eq!(
        app.chat_widget.session_draft,
        Some(SessionDraft::AwaitingSignIn(
            codex_app_server_protocol::AccountProvider::ClaudeSubscription
        ))
    );
    app.handle_sign_in_recovery(
        &mut tui,
        &mut server,
        sign_in_required::RecoveryEvent::Retry(
            codex_app_server_protocol::AccountProvider::ClaudeSubscription,
        ),
    )
    .await?;
    assert_eq!(app.chat_widget.session_draft, Some(SessionDraft::Starting));
    assert_eq!(
        serde_json::to_value(app.chat_widget.capture_thread_input_state())?["composer"],
        original_input
    );
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.start_draft_session(&server);
    app.start_draft_session(&server);
    let started = time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(AppEvent::StartupThreadStarted { result, .. }) = events.recv().await {
                break result;
            }
        }
    })
    .await??;
    assert_eq!(recorded_params(&requests, "thread/start").len(), 1);
    assert_eq!(
        started.session.model_provider_id,
        app.config.model_provider_id
    );
    server
        .request_handle()
        .request_typed::<codex_app_server_protocol::ThreadUnsubscribeResponse>(
            ClientRequest::ThreadUnsubscribe {
                request_id: codex_app_server_protocol::RequestId::String(
                    Uuid::new_v4().to_string(),
                ),
                params: codex_app_server_protocol::ThreadUnsubscribeParams {
                    thread_id: started.session.thread_id.to_string(),
                },
            },
        )
        .await?;
    server.shutdown().await?;
    proxy.abort();
    let _ = proxy.await;
    Ok(())
}

#[tokio::test]
async fn session_draft_reconnects_without_creating_or_submitting_a_thread() -> Result<()> {
    let (mut app, mut events, _ops) = Box::pin(make_test_app_with_channels()).await;
    let (mut server, _, proxy) = Box::pin(start_recording_remote_app_server(&app.config)).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = crate::resolve_remote_addr(&format!("ws://{}", listener.local_addr()?))?;
    let backend = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        disconnect::serve_reconnect_requests(tokio_tungstenite::accept_async(stream).await?, |_| {
            std::future::ready(None)
        })
        .await
    });
    app.chat_widget.session_draft = Some(SessionDraft::Editing);
    app.chat_widget.insert_str("Still unsent");
    app.app_server_target = AppServerTarget::Remote { endpoint };
    assert!(app.begin_reconnect());
    assert!(app.reconnect.presentation == reconnect::ReconnectPresentation::Draft);
    let mut tui = tui::test_support::make_test_tui()?;
    let connected = Box::pin(reconnect::reconnect(
        server.backend_id(),
        app.app_server_target.clone(),
        app.config.clone(),
        app.local_settings.clone(),
        /*thread_id*/ None,
        /*remote_cwd*/ None,
        reconnect::ReconnectPresentation::Draft,
    ))
    .await?;
    app.finish_reconnect(
        &mut tui,
        &mut server,
        &mut events,
        connected,
        CODEX_CLI_VERSION,
    )
    .await?;
    assert_eq!(app.chat_widget.session_draft, Some(SessionDraft::Editing));
    assert_eq!(app.chat_widget.composer_text_with_pending(), "Still unsent");
    server.shutdown().await?;
    let methods = time::timeout(Duration::from_secs(5), backend).await???;
    assert!(
        !methods
            .iter()
            .any(|method| matches!(method.as_str(), "thread/start" | "turn/start"))
    );
    proxy.abort();
    let _ = proxy.await;
    Ok(())
}
