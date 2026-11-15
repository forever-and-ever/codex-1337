use super::*;
use crate::collaboration_modes;
use codex_app_server_client::AppServerClient;
use codex_config::LoaderOverrides;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use crossterm::event::KeyCode;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn session_model_selection_preserves_defaults_and_updates_active_thread() -> Result<()> {
    for mode in [ModeKind::Default, ModeKind::Plan] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        app.config.model = Some("gpt-5.5".into());
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Medium);
        app.config.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::Low);
        let config_path = app.config.codex_home.join("config.toml");
        let original = "# My defaults\nmodel = 'gpt-5.5'\nmodel_reasoning_effort = 'medium'\nplan_mode_reasoning_effort = 'low'\n";
        std::fs::write(&config_path, original)?;
        let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
        let started = server.start_thread(&app.config).await?;
        let thread_id = started.session.thread_id;
        app.enqueue_primary_thread_session(started.session, started.turns)
            .await?;
        app.chat_widget
            .set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
        if mode == ModeKind::Plan {
            let mask = collaboration_modes::plan_mask(app.model_catalog.as_ref()).unwrap();
            app.chat_widget.set_collaboration_mask(mask);
        }
        // A real cyber-model choice reserves permission authority as well as model settings.
        let mut models = app.model_catalog.try_list_models()?;
        models
            .iter_mut()
            .find(|preset| preset.model == "gpt-5.6-terra")
            .expect("terra model")
            .model_specialty = Some(MODEL_SPECIALTY_CYBER.into());
        app.model_catalog = Arc::new(ModelCatalog::new(models));
        let mut tui = crate::tui::test_support::make_test_tui()?;
        for effort in [
            ReasoningEffortConfig::High,
            ReasoningEffortConfig::Ultra,
            ReasoningEffortConfig::Max,
        ] {
            while events.try_recv().is_ok() {}
            let command = match effort {
                ReasoningEffortConfig::High => {
                    "/model gpt-5.6-terra --reasoning high --speed=standard".into()
                }
                ReasoningEffortConfig::Ultra => "/model --reasoning=ultra".into(),
                ReasoningEffortConfig::Max => "/model --reasoning max".into(),
                _ => unreachable!(),
            };
            app.chat_widget.apply_external_edit(command);
            app.chat_widget.handle_key_event(KeyCode::Esc.into());
            app.chat_widget.handle_key_event(KeyCode::Enter.into());
            let previous = (
                app.chat_widget.current_model().to_owned(),
                app.chat_widget.current_reasoning_effort(),
                app.chat_widget.current_service_tier().map(str::to_owned),
            );
            let notification_first = effort == ReasoningEffortConfig::High;
            if notification_first {
                assert!(!app.chat_widget.hold_automatic_model_switch());
            }
            let mut messages = Vec::new();
            while let Ok(event) = events.try_recv() {
                if let AppEvent::InsertHistoryCell(cell) = &event {
                    messages.push(lines_to_single_string(&cell.transcript_lines(/*width*/ 80)));
                }
                Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
            }
            // UI dispatch only admits the request. No part of the choice is projected early.
            assert_eq!(
                (
                    app.chat_widget.current_model().to_owned(),
                    app.chat_widget.current_reasoning_effort(),
                    app.chat_widget.current_service_tier().map(str::to_owned),
                ),
                previous,
            );
            let settings = if notification_first {
                assert!(app.pending_server_profiles.contains_key(&thread_id));
                let settings = next_thread_settings_updated(&mut server, thread_id).await;
                app.enqueue_thread_notification(
                    thread_id,
                    ServerNotification::ThreadSettingsUpdated(settings.clone()),
                )
                .await?;
                assert!(!app.pending_server_profiles.contains_key(&thread_id));
                assert!(app.chat_widget.automatic_model_switch_pending());
                for event in std::iter::from_fn(|| events.try_recv().ok()) {
                    assert!(!matches!(event, AppEvent::CyberModelAutoReviewNotice));
                    if let AppEvent::InsertHistoryCell(cell) = event {
                        assert!(
                            !lines_to_single_string(&cell.transcript_lines(/*width*/ 80))
                                .contains("Model changed to"),
                            "notification cannot publish the model receipt confirmation",
                        );
                    }
                }
                app.settle_turn_submissions(&mut server).await?;
                assert!(!app.chat_widget.automatic_model_switch_pending());
                assert_eq!(
                    app.chat_widget.current_collaboration_mode().model(),
                    "gpt-5.6-terra"
                );
                let mut notices = 0;
                for event in std::iter::from_fn(|| events.try_recv().ok()) {
                    match event {
                        AppEvent::CyberModelAutoReviewNotice => notices += 1,
                        AppEvent::InsertHistoryCell(cell) => {
                            messages
                                .push(lines_to_single_string(&cell.transcript_lines(/*width*/ 80)));
                        }
                        _ => {}
                    }
                }
                assert_eq!(
                    notices, 1,
                    "retired profile authority must not revoke model acceptance"
                );
                assert_eq!(
                    messages
                        .iter()
                        .filter(|message| message.contains("Model changed to"))
                        .count(),
                    1,
                );
                settings
            } else {
                app.settle_turn_submissions(&mut server).await?;
                next_thread_settings_updated(&mut server, thread_id).await
            };
            assert_eq!(
                (
                    settings.thread_settings.model.as_str(),
                    settings.thread_settings.effort.clone(),
                    settings.thread_settings.service_tier.as_deref(),
                ),
                ("gpt-5.6-terra", Some(effort.clone()), Some("default"))
            );
            if mode == ModeKind::Default && effort == ReasoningEffortConfig::High {
                for command in [
                    "/model gpt-5.6-terra fast",
                    "/model gpt-5.6-terra --speeed=fast",
                    "/model gpt-5.6-terra --reasoning=invalid",
                    "/model --speed",
                    "/model --reasoning=",
                    "/model --reasoning --speed=standard",
                    "/model --speed=fast --speed standard",
                ] {
                    app.chat_widget.apply_external_edit(command.into());
                    app.chat_widget.handle_key_event(KeyCode::Esc.into());
                    app.chat_widget.handle_key_event(KeyCode::Enter.into());
                }
                while let Ok(event) = events.try_recv() {
                    assert!(!matches!(
                        event,
                        AppEvent::SelectSessionModel { .. }
                            | AppEvent::AstraSelectedFromModelPicker { .. }
                    ));
                    if let AppEvent::InsertHistoryCell(cell) = &event {
                        messages.push(lines_to_single_string(&cell.transcript_lines(/*width*/ 80)));
                    }
                    Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
                }
                insta::assert_snapshot!("session_only_model_confirmation", messages.join("\n"));
            }
            if !notification_first {
                app.enqueue_thread_notification(
                    thread_id,
                    ServerNotification::ThreadSettingsUpdated(settings),
                )
                .await?;
            }
            assert_eq!(
                (
                    app.chat_widget.current_model(),
                    app.chat_widget.current_reasoning_effort()
                ),
                ("gpt-5.6-terra", Some(effort.clone()))
            );
            let effective_mode = app.chat_widget.effective_collaboration_mode();
            assert_eq!(effective_mode.mode, mode);
            assert_eq!(effective_mode.model(), "gpt-5.6-terra");
            assert_eq!(effective_mode.reasoning_effort(), Some(effort));
            assert_eq!(app.chat_widget.current_service_tier(), Some("default"));
            assert_eq!(std::fs::read(&config_path)?, original.as_bytes());
        }
        let fresh = server.start_thread(&app.config).await?;
        assert_eq!(
            (fresh.session.model.as_str(), fresh.session.reasoning_effort),
            ("gpt-5.5", Some(ReasoningEffortConfig::Medium))
        );
        assert_eq!(
            app.config.plan_mode_reasoning_effort,
            Some(ReasoningEffortConfig::Low)
        );
        server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn standard_speed_selection_falls_back_for_older_app_servers() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    app.config.service_tier = Some("flex".into());
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let started = server.start_thread(&app.config).await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    app.chat_widget.set_service_tier(Some("flex".into()));
    server.thread_settings_update_supported = false;
    while events.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}

    app.select_session_model(
        &mut server,
        "gpt-5.6-terra".into(),
        Some(ReasoningEffortConfig::High),
        Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.into()),
        /*sparkle_model*/ None,
    )
    .await;

    assert_eq!(
        app.chat_widget.current_service_tier(),
        Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE)
    );
    assert_eq!(app.config.service_tier.as_deref(), Some("flex"));
    app.chat_widget.apply_external_edit("continue".into());
    app.chat_widget.handle_key_event(KeyCode::Esc.into());
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    assert_matches!(
        next_user_turn_op(&mut ops),
        Op::UserTurn { service_tier, .. }
            if service_tier == Some(Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.into()))
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn model_default_saves_report_server_outcomes_and_target_server_profile() -> Result<()> {
    for outcome in ["saved", "overridden", "rejected"] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Medium);
        let local_config_path = app.config.codex_home.join("config.toml");
        let local_config_before = std::fs::read(&local_config_path).ok();
        let server_home = tempfile::tempdir()?;
        let base_path = server_home.path().join("config.toml");
        let profile_path = server_home.path().join("work.config.toml");
        std::fs::write(&base_path, "# Base configuration stays unchanged.\n")?;
        std::fs::write(&profile_path, "")?;
        let mut loader_overrides = LoaderOverrides::without_managed_config_for_tests();
        loader_overrides.user_config_path =
            Some(AbsolutePathBuf::from_absolute_path(&profile_path)?);
        loader_overrides.user_config_profile = Some("work".parse()?);
        let overrides = if outcome == "overridden" {
            vec![
                ("model".into(), toml::Value::String("gpt-5.6-terra".into())),
                (
                    "model_reasoning_effort".into(),
                    toml::Value::String("low".into()),
                ),
                (
                    "plan_mode_reasoning_effort".into(),
                    toml::Value::String("low".into()),
                ),
                ("service_tier".into(), toml::Value::String("flex".into())),
            ]
        } else {
            Vec::new()
        };
        let config = ConfigBuilder::default()
            .codex_home(server_home.path().to_path_buf())
            .cli_overrides(overrides.clone())
            .loader_overrides(loader_overrides.clone())
            .harness_overrides(ConfigOverrides {
                cwd: Some(server_home.path().to_path_buf()),
                ..Default::default()
            })
            .build()
            .await?;
        let client = crate::start_embedded_app_server(
            codex_arg0::Arg0DispatchPaths::default(),
            config,
            overrides,
            loader_overrides,
            /*strict_config*/ false,
            codex_config::CloudConfigBundleLoader::default(),
            /*state_db*/ None,
            Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
            Default::default(),
        )
        .await?;
        let mut server = AppServerSession::new(
            crate::backend_id::BackendId::new(),
            AppServerClient::InProcess(client),
            crate::app_server_session::ThreadParamsMode::Remote,
        );
        if outcome == "rejected" {
            std::fs::write(&profile_path, "[broken")?;
        }
        while events.try_recv().is_ok() {}
        let mut tui = crate::tui::test_support::make_test_tui()?;
        for event in [
            AppEvent::PersistModelSelection {
                model: "gpt-5.5".into(),
                effort: Some(ReasoningEffortConfig::High),
            },
            AppEvent::PersistPlanModeReasoningEffort(Some(ReasoningEffortConfig::High)),
            AppEvent::ApplyAdvancedReasoning {
                model: "gpt-5.5".into(),
                effort: ReasoningEffortConfig::Ultra,
            },
        ] {
            Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
            // Config acknowledgement is not an input boundary: editing continues before apply.
            assert!(!app.server_operations.is_empty());
            app.chat_widget.insert_str("still responsive");
            assert_eq!(
                app.chat_widget.composer_text_with_pending(),
                "still responsive"
            );
            app.chat_widget.apply_external_edit(String::new());
            app.settle_server_ui_requests(&mut tui, &mut server).await?;
        }
        let messages = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.transcript_lines(/*width*/ 80)))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            messages.matches("higher-priority").count(),
            if outcome == "overridden" { 3 } else { 0 }
        );
        assert_eq!(
            messages.matches("Failed to save").count(),
            if outcome == "rejected" { 3 } else { 0 }
        );
        if outcome == "overridden" {
            insta::assert_snapshot!("overridden_model_defaults", messages);
        }
        let persisted = std::fs::read_to_string(&profile_path)?;
        if outcome == "rejected" {
            assert_eq!(persisted, "[broken");
            assert!(!messages.contains("Model changed"));
            assert!(!messages.contains("Service tier set"));
        } else {
            assert_eq!(
                toml::from_str::<toml::Value>(&persisted)?,
                toml::Value::Table(toml::toml! {
                    model = "gpt-5.5"
                    model_provider = "openai"
                    model_reasoning_effort = "medium"
                    plan_mode_reasoning_effort = "high"
                })
            );
        }
        assert_eq!(std::fs::read(&local_config_path).ok(), local_config_before);
        assert_eq!(
            std::fs::read_to_string(&base_path)?,
            "# Base configuration stays unchanged.\n"
        );
        // Runtime controls must work even when the defaults file cannot be parsed.
        let profile_before = std::fs::read(&profile_path)?;
        app.chat_widget
            .set_service_tier(Some(ServiceTier::Fast.request_value().into()));
        for event in [AppEvent::ServiceTierSelected {
            service_tier: Some(ServiceTier::Fast.request_value().into()),
        }] {
            Box::pin(app.handle_event(&mut tui, &mut server, event)).await?;
            assert!(app.server_operations.is_empty());
        }
        assert_eq!(
            app.config.service_tier.as_deref(),
            Some(ServiceTier::Fast.request_value())
        );
        assert_eq!(std::fs::read(&profile_path)?, profile_before);
        assert_eq!(std::fs::read(&local_config_path).ok(), local_config_before);
        server.shutdown().await?;
    }
    Ok(())
}
