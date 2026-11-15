//! Replacement starts use server defaults and preserve the current task on failure.

use super::*;
use codex_app_server_protocol::SkillMetadata;
use codex_app_server_protocol::SkillScope;
use codex_app_server_protocol::SkillsListEntry;
use codex_config::types::HistoryPersistence;
use core_test_support::context_snapshot;
use core_test_support::context_snapshot::ContextSnapshotOptions;
use core_test_support::responses;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn new_session_refreshes_account_for_the_started_provider() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let home = tempdir()?;
    app.config.codex_home = home.path().abs();
    app.config.sqlite = SqliteConfig::new_for_testing(home.path().abs());
    app.config.cli_auth_credentials_store_mode = codex_login::AuthCredentialsStoreMode::File;
    app_test_support::write_chatgpt_auth(
        home.path(),
        app_test_support::ChatGptAuthFixture::new("test-token")
            .email("user@example.com")
            .plan_type("pro"),
        codex_login::AuthCredentialsStoreMode::File,
    )
    .expect("write synthetic auth");
    let backend = wiremock::MockServer::start().await;
    let catalog_path = home.path().join("models.json");
    std::fs::write(
        &catalog_path,
        serde_json::to_vec(&codex_models_manager::bundled_models_response()?)?,
    )?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "model_provider = 'noauth-provider'\nmodel = 'test-model'\ncli_auth_credentials_store = 'file'\nchatgpt_base_url = {:?}\nmodel_catalog_json = {catalog_path:?}\nfeatures.apps = true\n[model_providers.noauth-provider]\nname = 'Test provider'\nbase_url = \"http://127.0.0.1:12345/v1\"\nrequires_openai_auth = false\nwire_api = 'responses'\n",
            backend.uri(),
        ),
    )?;
    let (mut server, _requests, proxy) = Box::pin(start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    ))
    .await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = server.next_event().await.expect("initial backend authority");
            let reported = matches!(&event.event, codex_app_server_client::AppServerEvent::ServerNotification(notification)
                if matches!(notification.as_ref(), ServerNotification::AccountUpdated(_)));
            app.handle_backend_event(&server, event).await;
            if reported { break; }
        }
    }).await?;
    assert!(app.chat_widget.has_codex_backend_auth());
    for provider in ["noauth-provider", "openai", "noauth-provider"] {
        crate::config_update::write_config_batch(
            server.request_handle(),
            vec![crate::config_update::replace_config_value(
                "model_provider",
                provider.into(),
            )],
        )
        .await?;
        *app.model_catalog.provider_write.lock().await = true;
        Box::pin(app.start_fresh_session(
            &mut tui,
            &mut server,
            /*session_start_source*/ None,
            /*initial_user_message*/ None,
            /*new_thread_name*/ None,
        ))
        .await;
        app.settle_navigation(&mut tui, &mut server).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while app.chat_widget.account_ready_reason().is_some() {
                let event = events.recv().await.expect("scoped payer completion");
                Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
            }
            Ok::<_, color_eyre::Report>(())
        })
        .await??;
        let chatgpt = provider == "openai";
        assert_eq!(
            (
                app.chat_widget.config_ref().model_provider_id.as_str(),
                app.chat_widget.has_chatgpt_account(),
                app.chat_widget.has_codex_backend_auth(),
                app.chat_widget.requires_openai_auth(),
                app.chat_widget.current_plan_type()
            ),
            (
                provider,
                chatgpt,
                true,
                chatgpt,
                chatgpt.then_some(codex_protocol::account::PlanType::Pro)
            )
        );
    }
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}

#[tokio::test]
async fn thread_account_uses_only_a_matching_server_default() -> Result<()> {
    let (mut server, mut commands, _service) = crate::app::tests::scripted_server::scripted_server(
        crate::app_server_session::ThreadParamsMode::Remote,
    )
    .await?;
    for (provider, current_provider) in [
        ("openai", "openai"),
        ("test-provider", "openai"),
        ("test-provider", "test-provider"),
        ("openai", "test-provider"),
    ] {
        let id = ThreadId::new();
        let mut session = test_thread_session(id, test_path_buf("/project"));
        session.model_provider_id = provider.to_string();
        server.model_provider_override = Some(current_provider.to_string());
        let (account, ()) = tokio::join!(server.read_thread_account(&session), async {
            let command = commands.recv().await.unwrap();
            let ClientRequest::GetAccount { params, .. } = command.request.as_ref() else {
                panic!("expected captured thread account");
            };
            assert_eq!(params.thread_id.as_deref(), Some(id.to_string().as_str()));
            assert_eq!(params.new_thread, None);
            command
                .response
                .send(Ok(Ok(serde_json::json!({
                    "account": (provider == "openai").then(|| serde_json::json!({
                        "type": "chatgpt", "email": "user@example.com", "planType": "pro"
                    })),
                    "requiresOpenaiAuth": provider == "openai"
                }))))
                .unwrap();
        });
        assert_eq!(account?.has_chatgpt_account(), provider == "openai");
        assert!(
            commands.try_recv().is_err(),
            "thread payer lookup must not consult ambient configuration"
        );
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn new_sessions_preserve_yolo_launch_and_later_permission_choices() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    app.harness_overrides.approval_policy = Some(AskForApproval::Never.to_core());
    app.harness_overrides.sandbox_mode =
        Some(codex_protocol::config_types::SandboxMode::DangerFullAccess);
    let (mut server, requests, proxy) = start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut loaded_config = app.config.clone();
    loaded_config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())?;
    loaded_config
        .permissions
        .set_permission_profile(PermissionProfile::read_only())?;
    let loaded = server.start_thread(&loaded_config).await?;
    app.select_agents_overview_thread_and_settle(&mut tui, &mut server, loaded.session.thread_id)
        .await?;
    for picker_change in [false, true] {
        if picker_change {
            app.chat_widget
                .set_approval_policy(AskForApproval::OnRequest);
            app.chat_widget
                .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
                    PermissionProfile::read_only(),
                    ActivePermissionProfile::new(":read-only"),
                ))?;
            app.adopt_server_permissions();
        }
        app.start_fresh_session_and_settle(
            &mut tui,
            &mut server,
            /*session_start_source*/ None,
            /*initial_user_message*/ None,
            /*new_thread_name*/ None,
        )
        .await;
        let cached = app.primary_session_configured.as_ref().unwrap().clone();
        let mut resumed = cached.clone();
        resumed.approval_policy = AskForApproval::OnRequest;
        resumed.permission_profile = PermissionProfile::read_only();
        resumed.active_permission_profile = None;
        app.restore_runtime_permissions(&mut resumed, &cached);
        assert_eq!(resumed, cached);
    }
    let params = recorded_params(&requests, "thread/start");
    assert_eq!(
        params
            .iter()
            .skip(1)
            .map(|params| serde_json::json!({
                "approvalPolicy": params["approvalPolicy"],
                "sandbox": params["sandbox"],
                "permissions": params["permissions"],
            }))
            .collect::<Vec<_>>(),
        vec![
            serde_json::json!({"approvalPolicy": "never", "sandbox": "danger-full-access", "permissions": null}),
            serde_json::json!({"approvalPolicy": "on-request", "sandbox": null, "permissions": ":read-only"}),
        ]
    );
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}

#[tokio::test]
async fn new_session_preserves_vim_line_yank() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let home = tempdir()?;
    app.config.codex_home = home.path().to_path_buf().abs();
    app.config.sqlite = SqliteConfig::new_for_testing(home.path().abs());
    app.chat_widget.toggle_vim_mode_and_notify();
    app.chat_widget.insert_str("saved line");
    for code in [KeyCode::Esc, KeyCode::Char('d'), KeyCode::Char('d')] {
        app.chat_widget
            .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }
    assert_eq!(app.chat_widget.composer_text_with_pending(), "");

    let (mut server, _requests, proxy) = start_recording_app_server_with_history(
        &app.config,
        HistoryCapabilities::Current,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
        crate::app_server_session::ThreadParamsMode::Embedded,
        LoaderOverrides::default(),
    )
    .await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.start_fresh_session_and_settle(
        &mut tui,
        &mut server,
        /*session_start_source*/ None,
        /*initial_user_message*/ None,
        /*new_thread_name*/ None,
    )
    .await;

    app.chat_widget.toggle_vim_mode_and_notify();
    app.chat_widget.insert_str("new line");
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "new line\nsaved line"
    );
    let composer_lines = render_bottom_popup(&app.chat_widget, /*width*/ 80)
        .lines()
        .skip_while(|line| !line.starts_with('›'))
        .take(2)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(composer_lines);
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}

#[tokio::test]
async fn widget_replacement_preserves_mention_usage_with_history_disabled() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    app.local_settings.history.persistence = HistoryPersistence::None;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let skills = SkillsListResponse {
        data: vec![SkillsListEntry {
            cwd: app.config.cwd.to_path_buf(),
            skills: ["statsig", "status"]
                .into_iter()
                .map(|name| SkillMetadata {
                    name: name.to_owned(),
                    description: String::new(),
                    short_description: None,
                    interface: None,
                    dependencies: None,
                    path: app
                        .config
                        .cwd
                        .to_path_buf()
                        .join(name)
                        .join("SKILL.md")
                        .abs()
                        .into(),
                    scope: SkillScope::User,
                    enabled: true,
                    plugin_id: None,
                })
                .collect(),
            errors: Vec::new(),
        }],
    };
    let init = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui,
        app.config.clone(),
        /*initial_user_message*/ None,
    );
    app.replace_chat_widget(ChatWidget::new_with_app_event(init));
    app.chat_widget.set_skills_from_response(&skills);
    app.chat_widget.insert_str("$status");
    app.chat_widget.handle_key_event(KeyCode::Enter.into());

    let init = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui,
        app.config.clone(),
        /*initial_user_message*/ None,
    );
    app.replace_chat_widget(ChatWidget::new_with_app_event(init));
    app.chat_widget.set_skills_from_response(&skills);
    app.chat_widget.insert_str("$stat");
    insta::assert_snapshot!(normalize_snapshot_paths(render_bottom_popup(
        &app.chat_widget,
        /*width*/ 80
    )));
    assert!(
        !app.local_settings
            .codex_home
            .join("mention_usage.json")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn replacement_uses_server_defaults_and_preserves_explicit_launch_settings() -> Result<()> {
    for (managed_effort, explicit, expected_model, expected_effort) in [
        (None, "saved", "server-model", "high"),
        (Some("medium"), "model", "cli-model", "high"),
        (Some("medium"), "effort", "server-model", "low"),
        (None, "profile_model", "profile-model", "high"),
        (None, "profile_effort", "server-model", "low"),
        (None, "profile_summary", "server-model", "high"),
        (None, "profile_summary_off", "server-model", "high"),
        (None, "profile_summary_cli", "server-model", "high"),
        (Some("medium"), "profile_model", "profile-model", "high"),
        (Some("medium"), "profile_effort", "server-model", "low"),
        (Some(""), "profile", "profile-model", "low"),
        (Some("medium"), "profile", "profile-model", "low"),
        (
            Some("medium"),
            "profile_unrelated",
            "managed-model",
            "medium",
        ),
    ] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let server_home = tempdir()?;
        let client_home = tempdir()?;
        std::fs::write(
            server_home.path().join("work.config.toml"),
            "model = \"server-model\"\nmodel_reasoning_effort = \"high\"\n",
        )?;
        std::fs::write(
            client_home.path().join("config.toml"),
            "model = \"stale-client-model\"\nmodel_reasoning_effort = \"low\"\nmodel_reasoning_summary = \"detailed\"\nfeatures.concurrent_reasoning_summaries = true\n",
        )?;
        if let Some(effort) = managed_effort {
            let effort = if effort.is_empty() {
                String::new()
            } else {
                format!("model_reasoning_effort = {effort:?}\n")
            };
            std::fs::write(
                server_home.path().join("requirements.toml"),
                format!("[models.new_thread]\nmodel = \"managed-model\"\n{effort}"),
            )?;
        }
        let mut server_config = app.config.clone();
        server_config.codex_home = server_home.path().to_path_buf().abs();
        server_config.sqlite = SqliteConfig::new_for_testing(server_home.path().abs());
        let (mut server, requests, proxy) = start_recording_app_server_with_history(
            &server_config,
            HistoryCapabilities::Current,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            if explicit.starts_with("profile_summary") {
                crate::app_server_session::ThreadParamsMode::Embedded
            } else {
                crate::app_server_session::ThreadParamsMode::Remote
            },
            LoaderOverrides {
                user_config_path: Some(server_home.path().join("work.config.toml").abs()),
                user_config_profile: Some("work".parse()?),
                system_requirements_path: Some(server_home.path().join("requirements.toml")),
                ..LoaderOverrides::default()
            },
        )
        .await?;
        server = server.with_remote_cwd_override(Some(server_config.cwd.to_path_buf()));
        server.bootstrap(&server_config).await?;
        app.config.codex_home = client_home.path().to_path_buf().abs();
        app.config.sqlite = SqliteConfig::new_for_testing(client_home.path().abs());
        app.chat_widget
            .handle_thread_session_quiet(test_thread_session(
                ThreadId::new(),
                client_home.path().to_path_buf(),
            ));
        match explicit {
            "model" => app.harness_overrides.model = Some("cli-model".to_string()),
            "effort" => app.cli_kv_overrides.push((
                "model_reasoning_effort".to_string(),
                TomlValue::String("low".to_string()),
            )),
            profile @ ("profile"
            | "profile_model"
            | "profile_effort"
            | "profile_unrelated"
            | "profile_summary"
            | "profile_summary_off"
            | "profile_summary_cli") => {
                let path = client_home.path().join("work.config.toml");
                std::fs::write(
                    &path,
                    match profile {
                        "profile_model" => "model = \"profile-model\"\n",
                        "profile_effort" => "model_reasoning_effort = \"low\"\n",
                        "profile_unrelated" => "model_verbosity = \"low\"\n",
                        "profile_summary" => {
                            "model_reasoning_summary = \"detailed\"\nfeatures.concurrent_reasoning_summaries = true\n"
                        }
                        "profile_summary_off" | "profile_summary_cli" => {
                            "model_reasoning_summary = \"detailed\"\nfeatures.concurrent_reasoning_summaries = false\n"
                        }
                        _ => "model = \"profile-model\"\nmodel_reasoning_effort = \"low\"\n",
                    },
                )?;
                app.loader_overrides.user_config_path = Some(path.abs());
                app.loader_overrides.user_config_profile = Some("work".parse()?);
            }
            _ => {}
        }
        if explicit == "profile_summary_cli" {
            app.cli_kv_overrides.push((
                "features.concurrent_reasoning_summaries".into(),
                true.into(),
            ));
        }
        if explicit.starts_with("profile_summary") {
            app.app_server_target = AppServerTarget::LocalDaemon {
                allow_embedded_fallback: true,
                endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
                    socket_path: client_home.path().join("daemon.sock").abs(),
                },
            };
            app.open_agents_overview(&server);
        }
        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.start_fresh_session_and_settle(
            &mut tui,
            &mut server,
            /*session_start_source*/ None,
            /*initial_user_message*/ None,
            /*new_thread_name*/ None,
        )
        .await;
        let starts = recorded_params(&requests, "thread/start");
        assert_eq!(starts.len(), 1);
        assert_eq!(
            (
                starts[0]["config"].get("model_reasoning_summary"),
                starts[0]["config"]["features"].get("concurrent_reasoning_summaries"),
            ),
            (
                explicit
                    .starts_with("profile_summary")
                    .then_some(&serde_json::json!("detailed")),
                explicit
                    .starts_with("profile_summary")
                    .then_some(&serde_json::json!(explicit != "profile_summary_off")),
            ),
            "{explicit}",
        );
        assert_eq!(
            (
                &starts[0]["model"],
                &starts[0]["config"]["model_reasoning_effort"]
            ),
            (
                &serde_json::json!(expected_model),
                &serde_json::json!(expected_effort)
            ),
        );
        assert_eq!(
            recorded_params(&requests, "config/read"),
            vec![
                serde_json::json!({"cwd": if explicit.starts_with("profile_summary") {
                    client_home.path().display().to_string()
                } else {
                    server_config.cwd.display().to_string()
                }, "includeLayers": true})
            ],
        );
        if explicit == "saved" {
            let rendered = render_bottom_popup(&app.chat_widget, /*width*/ 140)
                .replace(&server_config.cwd.display().to_string(), "<PROJECT>");
            insta::assert_snapshot!(rendered, @"
              • Ready

            ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

            › Ask Codex to do anything
                                                                                                                                                      󰜗
              server-model high · <PROJECT>
              ← for agents · ? for shortcuts
            ");
        }
        if explicit.starts_with("profile_summary") {
            // Opening the overview starts background RPCs. Let them finish before
            // closing the client so the proxy is not writing to a closed socket.
            loop {
                let event = events.recv().await.expect("agents overview refresh");
                if matches!(event, AppEvent::AgentsOverviewThreadsLoaded { .. }) {
                    break;
                }
            }
        }
        server.shutdown().await?;
        proxy.await??;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_implementation_reads_approved_plan_file_in_fresh_session() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    // Match the canonical CODEX_HOME used at launch.
    app.config.codex_home = app.config.codex_home.canonicalize()?;
    let model = responses::start_mock_server().await;
    app_test_support::MockResponsesConfig::new(&model.uri())
        // Exercise the local executor's file read without requiring a platform sandbox helper.
        .with_sandbox_mode("danger-full-access")
        .with_root_config("instructions = 'Help the user.'")
        .write(app.config.codex_home.as_path())?;
    let workspace = tempdir()?;
    app.config = app
        .rebuild_config_for_cwd(workspace.path().canonicalize()?)
        .await?;
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let plan = format!(
        "# Plan\n{}\n- Verify the final step.\n",
        "- Update the parser.\n".repeat(3_000)
    );
    let source = server.start_thread(&app.config).await?;
    let source_id = source.session.thread_id;
    app.enqueue_primary_thread_session(source.session, source.turns)
        .await?;
    Box::pin(app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::ImplementPlanInFreshSession { plan: plan.clone() },
    ))
    .await?;
    app.settle_navigation(&mut tui, &mut server).await?;
    let thread_id = app.chat_widget.thread_id().expect("fresh thread");
    assert_ne!(thread_id, source_id);
    let attachments = app.config.codex_home.join("attachments");
    let mut dirs = std::fs::read_dir(&attachments)?;
    let path = dirs
        .next()
        .expect("attachment directory")?
        .path()
        .join("approved-plan.md");
    assert!(dirs.next().is_none());
    assert_eq!(std::fs::read(&path)?, plan.as_bytes());
    let path = path.to_str().expect("UTF-8 attachment path");
    let command = if cfg!(windows) {
        format!(
            "Get-Content -LiteralPath '{}' -Tail 1",
            path.replace("'", "''")
        )
    } else {
        format!("tail -n 1 {}", shlex::try_quote(path)?)
    };
    let read_args = serde_json::json!({ "cmd": command });
    let response = responses::mount_sse_sequence(
        &model,
        vec![
            responses::sse(vec![
                responses::ev_response_created("read"),
                responses::ev_function_call("read_plan", "exec_command", &read_args.to_string()),
                responses::ev_completed("read"),
            ]),
            responses::sse_completed("implemented"),
        ],
    )
    .await;

    let mut user_rows = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            AppEvent::InsertHistoryCell(cell) if cell.as_any().is::<UserHistoryCell>() => {
                user_rows
                    .push(lines_to_single_string(&cell.raw_lines()).replace(path, "<PLAN_FILE>"));
            }
            event @ (AppEvent::SubmitThreadOp { .. } | AppEvent::CodexOp(_)) => {
                Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
            }
            _ => {}
        }
    }
    insta::assert_snapshot!(user_rows.join("\n"), @"Read the approved plan at <PLAN_FILE> and implement it.");
    app.settle_turn_submissions(&mut server).await?;
    super::safety_buffering::wait_for_turn_completed(&mut app, &mut server, thread_id).await;
    let requests = response.requests();
    let [initial, after_read] = requests.as_slice() else {
        panic!("expected a plan read and a follow-up request")
    };
    let output = after_read
        .function_call_output_text("read_plan")
        .expect("plan read result");
    assert!(output.contains("- Verify the final step."), "{output}");
    let mut body = initial.body_json();
    let prompt = body["input"].as_array_mut().unwrap().last_mut().unwrap();
    assert_eq!(prompt["role"], "user");
    let text = &mut prompt["content"][0]["text"];
    *text = text.as_str().unwrap().replace(path, "<PLAN_FILE>").into();
    insta::assert_snapshot!(
        "clear_context_plan_request",
        context_snapshot::format_context_snapshot(
            "Clear context and implement the approved plan in a fresh thread.",
            &[context_snapshot::SnapshotEntry::body(&body)],
            &ContextSnapshotOptions::default().rewrite_known_segments(),
        )
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn plan_implementation_handles_unavailable_attachment() -> Result<()> {
    for (write_fails, oversized) in [(true, false), (false, false), (false, true)] {
        crate::boxed_future!(assert_plan_implementation_with_unavailable_attachment(
            write_fails,
            oversized,
        ))
        .await?;
    }
    Ok(())
}

async fn assert_plan_implementation_with_unavailable_attachment(
    write_fails: bool,
    oversized: bool,
) -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let model = responses::start_mock_server().await;
    let response = responses::mount_sse_once(&model, responses::sse_completed("implemented")).await;
    let attachments = app.config.codex_home.join("attachments");
    let config = app_test_support::MockResponsesConfig::new(&model.uri());
    let config = if write_fails {
        std::fs::write(&attachments, "")?;
        config.with_sandbox_mode("danger-full-access")
    } else {
        let denied_path = serde_json::to_string(attachments.as_path())?;
        config
            // The embedded test executor has no sandbox helper for AGENTS.md discovery.
            .with_root_config("default_permissions = 'no-attachments'\nproject_doc_max_bytes = 0")
            .with_extra_config(&format!(
                "[permissions.no-attachments.filesystem]\n\":root\" = 'read'\n{denied_path} = 'deny'"
            ))
    };
    config.write(app.config.codex_home.as_path())?;
    let workspace = tempdir()?;
    app.config = app
        .rebuild_config_for_cwd(workspace.path().canonicalize()?)
        .await?;
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let source = server.start_thread(&app.config).await?;
    let source_id = source.session.thread_id;
    app.enqueue_primary_thread_session(source.session, source.turns)
        .await?;
    let steps = if oversized { 3_000 } else { 1_000 };
    let plan = format!(
        "# Plan\n{}- Verify the final step.",
        "- Update the parser.\n".repeat(steps)
    );
    crate::boxed_future!(app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::ImplementPlanInFreshSession { plan: plan.clone() },
    ))
    .await?;

    app.settle_navigation(&mut tui, &mut server).await?;
    if !write_fails {
        let path = std::fs::read_dir(&attachments)?
            .next()
            .expect("the app server saved the plan")?
            .path()
            .join("approved-plan.md");
        assert_eq!(std::fs::read_to_string(path)?, plan);
    }
    let thread_id = app.chat_widget.thread_id().expect("current thread");
    if oversized {
        assert_eq!(thread_id, source_id);
        assert!(response.requests().is_empty());
    } else {
        assert_ne!(thread_id, source_id);
        while let Ok(event) = events.try_recv() {
            if matches!(
                event,
                AppEvent::SubmitThreadOp { .. } | AppEvent::CodexOp(_)
            ) {
                crate::boxed_future!(app.handle_event(&mut tui, &mut server, event)).await?;
            }
        }
        app.settle_turn_submissions(&mut server).await?;
        super::safety_buffering::wait_for_turn_completed(&mut app, &mut server, thread_id).await;
        let request = response.single_request();
        let messages = request.inputs_of_type("message");
        let user = messages.last().expect("implementation prompt");
        assert_eq!(user["role"], "user");
        assert_eq!(user["content"][0]["text"], "Implement the plan.");
        let recovered = request
            .inputs_of_type("function_call_output")
            .iter()
            .filter(|item| item["name"] == "proposed_plan")
            .map(|item| item["output"].as_str().expect("plan chunk"))
            .collect::<String>();
        assert_eq!(recovered, plan);
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn replacement_failure_keeps_current_task_and_restores_input() -> Result<()> {
    for fail_read in [false, true] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let home = tempdir()?;
        app.config.codex_home = home.path().to_path_buf().abs();
        app.config.sqlite = SqliteConfig::new_for_testing(home.path().abs());
        let current = ThreadId::new();
        app.enqueue_primary_thread_session(
            test_thread_session(current, app.config.cwd.to_path_buf()),
            Vec::new(),
        )
        .await?;
        while events.try_recv().is_ok() {}
        let original_config = app.config.clone();
        let (mut server, requests, proxy) = start_recording_app_server_with_history(
            &app.config,
            HistoryCapabilities::ThreadStartFails,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
        if fail_read {
            std::fs::write(home.path().join("config.toml"), "invalid = [")?;
        }
        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.start_fresh_session_and_settle(
            &mut tui,
            &mut server,
            /*session_start_source*/ None,
            Some("keep this request".into()),
            /*new_thread_name*/ None,
        )
        .await;
        assert_eq!(app.chat_widget.thread_id(), Some(current));
        assert_eq!(app.active_thread_id, Some(current));
        assert!(app.active_thread_rx.is_some());
        assert_eq!(app.config, original_config);
        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            "keep this request"
        );
        assert!(recorded_params(&requests, "thread/unsubscribe").is_empty());
        assert_eq!(
            recorded_params(&requests, "thread/start").len(),
            usize::from(!fail_read)
        );
        let errors = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.display_lines(/*width*/ 1000)))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if fail_read {
            insta::assert_snapshot!(errors, @"■ Failed to read new session defaults: config/read failed in TUI");
        } else {
            insta::assert_snapshot!(errors, @"■ Failed to start a fresh session through the app server: thread/start failed during TUI bootstrap: thread/start failed: replacement unavailable (code -32603)");
        }
        server.shutdown().await?;
        proxy.await??;
    }
    Ok(())
}

#[tokio::test]
async fn replacement_preserves_remote_launch_paths_and_older_servers() -> Result<()> {
    for (remote_cwd, capabilities) in [
        (None, HistoryCapabilities::Current),
        (Some(PathBuf::from(".")), HistoryCapabilities::Current),
        (None, HistoryCapabilities::ConfigReadUnsupported(-32601)),
        (None, HistoryCapabilities::ConfigReadUnsupported(-32600)),
    ] {
        let (mut app, _events, _ops) = make_test_app_with_channels().await;
        let home = tempdir()?;
        app.config.codex_home = home.path().to_path_buf().abs();
        app.config.sqlite = SqliteConfig::new_for_testing(home.path().abs());
        std::fs::write(
            home.path().join("config.toml"),
            "model = \"configured-model\"\n",
        )?;
        app.enqueue_primary_thread_session(
            test_thread_session(ThreadId::new(), app.config.cwd.to_path_buf()),
            Vec::new(),
        )
        .await?;
        let (server, requests, proxy) = start_recording_app_server_with_history(
            &app.config,
            capabilities,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Remote,
            LoaderOverrides::default(),
        )
        .await?;
        let mut server = server.with_remote_cwd_override(remote_cwd.clone());
        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.start_fresh_session_and_settle(
            &mut tui,
            &mut server,
            /*session_start_source*/ None,
            /*initial_user_message*/ None,
            /*new_thread_name*/ None,
        )
        .await;
        assert_eq!(
            recorded_params(&requests, "config/read"),
            vec![serde_json::json!({"cwd": ".", "includeLayers": true}),]
        );
        let starts = recorded_params(&requests, "thread/start");
        assert_eq!(starts.len(), 1);
        assert_eq!(
            (&starts[0]["cwd"], &starts[0]["model"]),
            (
                &serde_json::json!(remote_cwd),
                &serde_json::json!("configured-model")
            ),
        );
        assert_eq!(app.chat_widget.current_model(), "configured-model");
        server.shutdown().await?;
        proxy.await??;
    }
    Ok(())
}

#[tokio::test]
async fn provider_continuation_starts_current_provider_and_drafts_saved_reference() -> Result<()> {
    use crate::app_server_session::ThreadParamsMode;
    for (mode, capabilities) in [
        (ThreadParamsMode::Embedded, HistoryCapabilities::Current),
        (ThreadParamsMode::Remote, HistoryCapabilities::Current),
        (
            ThreadParamsMode::Embedded,
            HistoryCapabilities::ThreadStartFails,
        ),
    ] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let home = tempdir()?;
        app.config.codex_home = home.path().to_path_buf().abs();
        app.config.sqlite = SqliteConfig::new_for_testing(home.path().abs());
        std::fs::write(
            home.path().join("config.toml"),
            "model = 'current-model'\n[tui]\nresume_cwd = 'current'\n",
        )?;
        crate::legacy_core::config::set_project_trust_level(
            home.path(),
            app.config.cwd.as_path(),
            codex_protocol::config_types::TrustLevel::Trusted,
        )
        .map_err(std::io::Error::other)?;
        let source_id = ThreadId::from_string(
            &create_fake_rollout(
                home.path(),
                "2025-01-05T12-00-00",
                "2025-01-05T12:00:00Z",
                "Saved provider history",
                Some("old-provider"),
                /*git_info*/ None,
            )
            .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?,
        )?;
        let (mut server, requests, proxy) = start_recording_app_server_with_history(
            &app.config,
            capabilities,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            mode,
            LoaderOverrides::default(),
        )
        .await?;
        server
            .thread_set_name(source_id, "Launch readiness".into())
            .await?;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        tui.set_owned_screen(matches!(mode, ThreadParamsMode::Embedded))?;
        let target = crate::resume_picker::SessionTarget {
            backend_id: server.backend_id(),
            path: None,
            thread_id: source_id,
            cwd: None,
            history_mode: None,
        };
        app.resume_target_session(&mut tui, &mut server, target)
            .await?;
        app.settle_navigation(&mut tui, &mut server).await?;
        assert!(app.chat_widget.has_provider_error());
        let path = app.chat_widget.rollout_path().expect("saved history path");
        let original_history = std::fs::read(&path)?;
        while events.try_recv().is_ok() {}
        app.handle_tui_event(
            &mut tui,
            &mut server,
            TuiEvent::Key(KeyCode::Char('c').into()),
        )
        .await?;
        let action = std::iter::from_fn(|| events.try_recv().ok())
            .find(|event| matches!(event, AppEvent::ContinueProviderChat { .. }))
            .expect("c starts a continuation");
        Box::pin(app.handle_event(&mut tui, &mut server, action)).await?;
        app.settle_navigation(&mut tui, &mut server).await?;
        if capabilities == HistoryCapabilities::ThreadStartFails {
            assert_eq!(app.chat_widget.thread_id(), Some(source_id));
            assert!(app.chat_widget.has_provider_error());
        } else {
            let new_id = app.chat_widget.thread_id().expect("new thread");
            assert_ne!(new_id, source_id);
            assert!(!app.chat_widget.has_provider_error());
            assert_eq!(app.chat_widget.current_model(), "current-model");
            assert_eq!(
                app.primary_session_configured
                    .as_ref()
                    .unwrap()
                    .model_provider_id,
                "openai"
            );
            let mut submissions = Vec::new();
            while let Ok(event) = events.try_recv() {
                match event {
                    AppEvent::SubmitThreadOp {
                        thread_id,
                        op: Op::UserTurn { items, .. },
                    } => {
                        assert_eq!(thread_id, new_id);
                        submissions.push(items);
                    }
                    AppEvent::CodexOp(Op::UserTurn { items, .. }) => submissions.push(items),
                    event => {
                        Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
                    }
                }
            }
            assert!(submissions.is_empty());
            let expected_draft = format!(
                "Read “Launch readiness” and let’s continue where we left off.\n\nSaved conversation: {}",
                path.display()
            );
            assert_eq!(app.chat_widget.composer_text_with_pending(), expected_draft);
            let composer_lines = render_bottom_popup(&app.chat_widget, /*width*/ 80)
                .lines()
                .skip_while(|line| !line.starts_with('›'))
                .take(2)
                .collect::<Vec<_>>()
                .join("\n");
            insta::assert_snapshot!("provider_continuation_draft", composer_lines);
            // A repeated queued key cannot create a second chat or send into the original.
            Box::pin(app.handle_event(
                &mut tui,
                &mut server,
                AppEvent::ContinueProviderChat {
                    thread_id: source_id,
                },
            ))
            .await?;
            assert_eq!(app.chat_widget.thread_id(), Some(new_id));
            assert_eq!(app.chat_widget.composer_text_with_pending(), expected_draft);

            let edited_draft = format!("{expected_draft}\n\nFocus on the remaining launch checks.");
            app.chat_widget.apply_external_edit(edited_draft.clone());
            app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(KeyCode::Enter.into()))
                .await?;
            while let Ok(event) = events.try_recv() {
                match event {
                    AppEvent::SubmitThreadOp {
                        thread_id,
                        op: Op::UserTurn { items, .. },
                    } => {
                        assert_eq!(thread_id, new_id);
                        submissions.push(items);
                    }
                    AppEvent::CodexOp(Op::UserTurn { items, .. }) => submissions.push(items),
                    event => {
                        Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
                    }
                }
            }
            assert_eq!(
                submissions,
                vec![vec![codex_app_server_protocol::UserInput::Text {
                    text: edited_draft,
                    text_elements: Vec::new(),
                }]]
            );
            assert_eq!(app.chat_widget.composer_text_with_pending(), "");
        }
        assert_eq!(recorded_params(&requests, "thread/start").len(), 1);
        assert!(recorded_params(&requests, "turn/start").is_empty());
        assert_eq!(std::fs::read(&path)?, original_history);
        tui.set_owned_screen(/*owned*/ false)?;
        server.shutdown().await?;
        proxy.await??;
    }
    Ok(())
}
