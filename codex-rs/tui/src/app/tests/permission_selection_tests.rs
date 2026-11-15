//! Permission selection through the existing app-server settings API.

use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn builtin_permission_selection_adopts_server_settings() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let started = server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "turn-1"),
        /*replay_kind*/ None,
    );
    assert!(app.chat_widget.is_user_turn_pending_or_running());
    app.runtime_approvals_reviewer_override = Some(ApprovalsReviewer::AutoReview);
    let before = RuntimePermissionProfileOverride::from_config(app.chat_widget.config_ref());
    let profile_id = if before
        .active_permission_profile
        .as_ref()
        .is_some_and(|profile| profile.id == ":workspace")
    {
        ":read-only"
    } else {
        ":workspace"
    };
    while events.try_recv().is_ok() {}
    app.select_permission_profile(
        &mut server,
        PermissionProfileSelection {
            profile_id: profile_id.into(),
            approval_policy: Some(AskForApproval::OnRequest),
            approvals_reviewer: Some(ApprovalsReviewer::User),
            display_label: profile_id.into(),
        },
    )
    .await;
    app.settle_turn_submissions(&mut server).await?;
    assert_eq!(
        RuntimePermissionProfileOverride::from_config(app.chat_widget.config_ref()),
        before,
    );
    assert!(
        app.agents_overview
            .requested_permission_profiles
            .contains_key(&thread_id)
    );
    insta::assert_snapshot!(next_history_message(&mut events).replace(profile_id, "<PROFILE>"), @"• Permission selection requested: <PROFILE>");
    // Another permissions menu and prompt are queued as this selection closes.
    app.chat_widget
        .set_queue_autosend_suppressed(/*suppressed*/ true);
    for text in ["/permissions", "after permissions"] {
        app.chat_widget
            .restore_user_message_to_composer(text.into());
        app.chat_widget.handle_key_event(KeyCode::Tab.into());
    }
    app.chat_widget.handle_server_notification(
        turn_completed_notification(thread_id, "turn-1", TurnStatus::Completed),
        /*replay_kind*/ None,
    );
    app.app_event_tx.send(AppEvent::SettingsSelectionClosed);
    let settings = next_thread_settings_updated(&mut server, thread_id).await;
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::ThreadSettingsUpdated(settings),
    )
    .await?;
    assert!(!app.pending_server_profiles.contains_key(&thread_id));
    assert_eq!(
        (
            app.chat_widget
                .config_ref()
                .permissions
                .active_permission_profile(),
            app.config.approvals_reviewer
        ),
        (
            Some(ActivePermissionProfile::new(profile_id)),
            ApprovalsReviewer::User
        ),
    );
    assert_eq!(
        app.runtime_permission_profile_override,
        Some(RuntimePermissionProfileOverride::from_config(&app.config))
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;
    loop {
        let event = events.try_recv().expect("queued permissions menu");
        if matches!(event, AppEvent::OpenPermissionsPopup) {
            // Neither the old picker's close nor its confirmation may release this prompt.
            assert!(app.chat_widget.has_queued_follow_up_messages());
            break;
        }
        app.handle_event(&mut tui, &mut server, event).await?;
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn matching_builtin_permission_selection_submits_pending_prompt() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let started = server.start_thread(&app.config).await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    app.chat_widget.initial_user_message =
        create_initial_user_message(Some("review this".to_string()), Vec::new(), Vec::new());
    let config = app.chat_widget.config_ref();
    let selection = PermissionProfileSelection {
        profile_id: config.permissions.active_permission_profile().unwrap().id,
        approval_policy: Some(config.permissions.approval_policy.value().into()),
        approvals_reviewer: Some(config.approvals_reviewer),
        display_label: "Current permissions".to_string(),
    };
    while events.try_recv().is_ok() {}
    app.select_permission_profile(&mut server, selection).await;
    assert!(app.chat_widget.initial_user_message.is_none());
    assert!(app.pending_server_profiles.is_empty());
    let Op::UserTurn { items, .. } = next_user_turn_op(&mut ops) else {
        panic!("expected initial user turn");
    };
    assert_eq!(
        items,
        vec![AppServerUserInput::Text {
            text: "review this".to_string(),
            text_elements: Vec::new(),
        }]
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn inline_server_permission_selection_holds_queued_prompt_until_confirmation() -> Result<()> {
    for notification_first in [false, true] {
        boxed_future!(async {
            let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
            let home = tempdir()?;
            std::fs::write(
                home.path().join("config.toml"),
                "default_permissions = \":workspace\"\n[permissions.server-only]\nextends = \":read-only\"\n",
            )?;
            let server_config = ConfigBuilder::default()
                .codex_home(home.path().into())
                .build()
                .await?;
            let mut server = crate::start_embedded_app_server_for_picker(&server_config).await?;
            let started = server.start_thread(&app.config).await?;
            let thread_id = started.session.thread_id;
            app.enqueue_primary_thread_session(started.session, started.turns)
                .await?;
            let mut tui = crate::tui::test_support::make_test_tui()?;
            let account_baseline = tokio::time::timeout(
                std::time::Duration::from_secs(/*secs*/ 2),
                async {
                    loop {
                        tokio::select! {
                            Some(event) = events.recv() => {
                                if matches!(&event, AppEvent::ConversationAccountLoaded { thread_id: loaded, .. } if *loaded == thread_id) {
                                    break Ok::<_, color_eyre::Report>(event);
                                }
                                app.handle_event(&mut tui, &mut server, event).await?;
                            }
                            Some(completion) = app.server_operations.control_rx.recv() => {
                                app.handle_server_completion(&mut tui, &mut server, completion).await?;
                            }
                            Some(completion) = app.thread_delivery.completion_rx.recv() => {
                                app.handle_local_thread_completion(&mut server, completion).await?;
                            }
                        }
                    }
                },
            ).await??;
            // Establish the conversation's first authoritative account scope before
            // requesting its permission catalog. A scope boundary intentionally cancels discovery.
            assert!(app.account_ui.displayed.is_none());
            app.handle_event(&mut tui, &mut server, account_baseline).await?;
            assert!(app.account_ui.displayed.as_ref().is_some_and(|(shown, _)| *shown == Some(thread_id)));
            app.settle_turn_submissions(&mut server).await?;
            while ops.try_recv().is_ok() {}

            app.chat_widget.select_permission_profile("server-only");
            assert!(app.chat_widget.queue_user_message_with_options(
                crate::chatwidget::UserMessage::from("use the selected profile"),
                crate::bottom_pane::QueuedInputAction::Plain,
                Vec::new(),
            ));
            let mut admission_trace = Vec::new();
            for _ in 0..20 {
                if app.pending_server_profiles.contains_key(&thread_id) {
                    break;
                }
                tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 2), async {
                    tokio::select! {
                        Some(event) = events.recv() => {
                            let event_description = format!("{event:?}").chars().take(512).collect::<String>();
                            let before = (
                                app.chat_widget.permission_popup_request_id(),
                                app.chat_widget.inline_permission_profile_selection_pending(),
                                app.chat_widget.thread_id(),
                                app.chat_widget.config_ref().cwd.clone(),
                                app.chat_widget.has_active_view(),
                            );
                            if let AppEvent::PermissionProfilesLoaded { result, .. } = &event {
                                let discovery = result.as_ref().map_err(|message| {
                                    color_eyre::eyre::eyre!("permission discovery failed: {message}")
                                })?;
                                assert!(
                                    discovery.profiles.iter().any(|profile| profile.id == "server-only"),
                                    "{discovery:?}"
                                );
                            }
                            app.handle_event(&mut tui, &mut server, event).await?;
                            let after = (
                                app.chat_widget.permission_popup_request_id(),
                                app.chat_widget.inline_permission_profile_selection_pending(),
                                app.chat_widget.thread_id(),
                                app.chat_widget.config_ref().cwd.clone(),
                                app.chat_widget.has_active_view(),
                            );
                            admission_trace.push(format!("{event_description}: before={before:?}, after={after:?}"));
                        }
                        Some(completion) = app.server_operations.control_rx.recv() => {
                            admission_trace.push("control completion".to_string());
                            app.handle_server_completion(&mut tui, &mut server, completion).await?;
                        }
                        Some(completion) = app.thread_delivery.completion_rx.recv() => {
                            admission_trace.push(format!("ordinary: {completion:?}")
                                .chars().take(256).collect());
                            app.handle_local_thread_completion(&mut server, completion).await?;
                        }
                    }
                    Ok::<_, color_eyre::Report>(())
                })
                .await
                .map_err(|error| color_eyre::eyre::eyre!(
                    "permission admission timed out: notification_first={notification_first}, pending={:?}, discovery={:?}, inline={}, trace={admission_trace:?}: {error}",
                    app.pending_server_profiles,
                    app.chat_widget.permission_popup_request_id(),
                    app.chat_widget.inline_permission_profile_selection_pending(),
                ))??;
            }
            let first_request = app.pending_server_profiles[&thread_id].request_id;
            // Run the old close/settlement events before the ordinary mutation receipt.
            // Admission must already belong to this request, not its eventual response.
            while let Ok(event) = events.try_recv() {
                app.handle_event(&mut tui, &mut server, event).await?;
            }
            let before_receipt = ops.try_recv();
            assert!(before_receipt.is_err(), "{before_receipt:?}");
            let turn_submissions::ThreadSubmissionPreview::Sending { active, waiting } =
                app.server_operations.thread_preview(Some(thread_id))
            else {
                panic!("permission mutation must own an ordered thread operation");
            };
            let permission_operation = std::iter::once(active)
                .chain(waiting)
                .find_map(|entry| match entry {
                    turn_submissions::SubmissionPreviewEntry::Operation { id, text: "Selecting permissions" } => Some(id),
                    _ => None,
                })
                .expect("the permission mutation must remain in the operation ledger");
            let receipt = tokio::time::timeout(
                std::time::Duration::from_secs(/*secs*/ 2),
                async {
                    loop {
                        let completion = app.thread_delivery.completion_rx.recv().await
                            .expect("permission mutations use the ordinary thread receipt lane");
                        if matches!(&completion,
                            super::super::thread_delivery::LocalThreadCompletion::Submission { id, .. }
                                if *id == permission_operation)
                        {
                            break Ok::<_, color_eyre::Report>(completion);
                        }
                        app.handle_local_thread_completion(&mut server, completion).await?;
                    }
                },
            )
            .await??;
            let settings = next_thread_settings_updated(&mut server, thread_id).await;

            if notification_first {
                app.enqueue_thread_notification(
                    thread_id,
                    ServerNotification::ThreadSettingsUpdated(settings.clone()),
                )
                .await?;
                assert!(!app.pending_server_profiles.contains_key(&thread_id));
                assert_eq!(
                    app.agents_overview
                        .selected_permission_profiles
                        .get(&thread_id)
                        .map(String::as_str),
                    Some("server-only"),
                );
                assert_eq!(
                    app.chat_widget
                        .config_ref()
                        .permissions
                        .active_permission_profile(),
                    Some(ActivePermissionProfile {
                        id: "server-only".into(),
                        extends: Some(":read-only".into()),
                    }),
                );
                // A custom-profile revalidation is a distinct request even with the same
                // profile name. Its reservation cannot belong to the delayed first receipt.
                app.select_permission_profile(
                    &mut server,
                    PermissionProfileSelection {
                        profile_id: "server-only".into(),
                        approval_policy: None,
                        approvals_reviewer: None,
                        display_label: "server-only".into(),
                    },
                )
                .await;
                let second_request = app.pending_server_profiles[&thread_id].request_id;
                assert_ne!(first_request, second_request);
                app.handle_local_thread_completion(&mut server, receipt)
                    .await?;
                assert_eq!(
                    app.pending_server_profiles[&thread_id].request_id,
                    second_request
                );
                assert!(
                    !app.agents_overview
                        .requested_permission_profiles
                        .contains_key(&thread_id)
                );
                while let Ok(event) = events.try_recv() {
                    app.handle_event(&mut tui, &mut server, event).await?;
                }
                assert!(ops.try_recv().is_err());
                app.settle_turn_submissions(&mut server).await?;
                assert!(!app.pending_server_profiles.contains_key(&thread_id));
                // Unchanged custom revalidation needs no new broadcast.
                assert!(
                    !app.agents_overview
                        .requested_permission_profiles
                        .contains_key(&thread_id)
                );
            } else {
                app.handle_local_thread_completion(&mut server, receipt)
                    .await?;
                assert!(!app.pending_server_profiles.contains_key(&thread_id));
                assert!(
                    app.agents_overview
                        .requested_permission_profiles
                        .contains_key(&thread_id)
                );
                while let Ok(event) = events.try_recv() {
                    app.handle_event(&mut tui, &mut server, event).await?;
                }
                let before_confirmation = ops.try_recv();
                assert!(
                    before_confirmation.is_err(),
                    "unexpected operation before explicit permission confirmation: {before_confirmation:?}"
                );
                app.enqueue_thread_notification(
                    thread_id,
                    ServerNotification::ThreadSettingsUpdated(settings.clone()),
                )
                .await?;
            }
            while let Ok(event) = events.try_recv() {
                app.handle_event(&mut tui, &mut server, event).await?;
            }
            let admitted = ops.try_recv();
            let current_model = app.chat_widget.current_model().to_owned();
            assert!(
                admitted.is_ok(),
                "permission admission produced no operation: order={notification_first}, pending={:?}, requested={:?}, popup={}, inline={}, modal={}, current_model={}, input={:?}, queued_events={}, admission_trace={admission_trace:?}",
                app.pending_server_profiles,
                app.agents_overview.requested_permission_profiles,
                app.chat_widget.permission_popup_pending(),
                app.chat_widget.inline_permission_profile_selection_pending(),
                app.chat_widget.has_active_view(),
                current_model,
                app.chat_widget.capture_thread_input_state(),
                events.len(),
            );
            let Op::UserTurn { items, .. } = admitted.unwrap() else {
                panic!("expected queued user turn");
            };
            assert_eq!(
                items,
                vec![AppServerUserInput::Text {
                    text: "use the selected profile".to_string(),
                    text_elements: Vec::new(),
                }]
            );
            // Neither another broadcast nor an obsolete receipt can republish this input.
            app.enqueue_thread_notification(
                thread_id,
                ServerNotification::ThreadSettingsUpdated(settings),
            )
            .await?;
            while let Ok(event) = events.try_recv() {
                app.handle_event(&mut tui, &mut server, event).await?;
            }
            assert!(ops.try_recv().is_err());

            let confirmed =
                RuntimePermissionProfileOverride::from_config(app.chat_widget.config_ref());
            app.chat_widget
                .set_queue_autosend_suppressed(/*suppressed*/ true);
            assert!(app.chat_widget.queue_user_message_with_options(
                crate::chatwidget::UserMessage::from("retain after rejected permissions"),
                crate::bottom_pane::QueuedInputAction::Plain,
                Vec::new(),
            ));
            app.select_permission_profile(
                &mut server,
                PermissionProfileSelection {
                    profile_id: "missing-on-server".into(),
                    approval_policy: None,
                    approvals_reviewer: None,
                    display_label: "missing-on-server".into(),
                },
            )
            .await;
            assert!(app.pending_server_profiles.contains_key(&thread_id));
            app.settle_turn_submissions(&mut server).await?;
            while let Ok(event) = events.try_recv() {
                assert!(
                    !matches!(
                        event,
                        AppEvent::SettingsSelectionClosed | AppEvent::SettingsSelectionSettled
                    ),
                    "rejection must not schedule permission admission",
                );
                app.handle_event(&mut tui, &mut server, event).await?;
            }
            assert!(!app.pending_server_profiles.contains_key(&thread_id));
            assert!(app.chat_widget.has_queued_follow_up_messages());
            assert_eq!(
                RuntimePermissionProfileOverride::from_config(app.chat_widget.config_ref()),
                confirmed,
            );
            assert!(ops.try_recv().is_err());
            server.shutdown().await?;
            Ok::<_, color_eyre::Report>(())
        })
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn custom_permission_selection_uses_server_definition_and_preserves_state_on_rejection()
-> Result<()> {
    boxed_future!(custom_permission_selection_scenario()).await
}

async fn custom_permission_selection_scenario() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.config.ephemeral = false;
    let home = tempdir()?;
    std::fs::write(
        home.path().join("config.toml"),
        "default_permissions = \":workspace\"\n[permissions.shared]\nextends = \":workspace\"\n",
    )?;
    let server_config = ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await?;
    let mut server = crate::start_embedded_app_server_for_picker(&server_config).await?;
    let started = server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    let client_home = tempdir()?;
    std::fs::write(
        client_home.path().join("config.toml"),
        "default_permissions = \":workspace\"\n[permissions.shared]\nextends = \":danger-full-access\"\n",
    )?;
    let client_config = ConfigBuilder::default()
        .codex_home(client_home.path().into())
        .build()
        .await?;
    app.config.codex_home = client_config.codex_home;
    app.config.custom_permission_profiles = client_config.custom_permission_profiles;
    let selection = PermissionProfileSelection {
        profile_id: "shared".into(),
        approval_policy: Some(AskForApproval::Never),
        approvals_reviewer: None,
        display_label: "shared".into(),
    };
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "turn-1"),
        /*replay_kind*/ None,
    );
    assert!(app.chat_widget.is_user_turn_pending_or_running());
    let before = RuntimePermissionProfileOverride::from_config(app.chat_widget.config_ref());
    app.select_permission_profile(&mut server, selection.clone())
        .await;
    app.settle_turn_submissions(&mut server).await?;
    assert_eq!(
        RuntimePermissionProfileOverride::from_config(app.chat_widget.config_ref()),
        before
    );
    let settings = next_thread_settings_updated(&mut server, thread_id).await;
    let expected = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
        &settings.thread_settings.sandbox_policy.to_core(),
        settings.thread_settings.cwd.as_path(),
    );
    assert!(matches!(
        settings.thread_settings.sandbox_policy,
        codex_app_server_protocol::SandboxPolicy::WorkspaceWrite { .. }
    ));
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::ThreadSettingsUpdated(settings),
    )
    .await?;
    assert_eq!(
        (
            app.chat_widget
                .config_ref()
                .permissions
                .permission_profile(),
            app.chat_widget
                .config_ref()
                .permissions
                .approval_policy
                .value()
        ),
        (&expected, AskForApproval::Never.to_core())
    );
    std::fs::write(
        home.path().join("config.toml"),
        "default_permissions = \":workspace\"\n[permissions.shared]\nextends = \":read-only\"\n[permissions.other]\nextends = \":read-only\"\n",
    )?;
    app.select_permission_profile(
        &mut server,
        PermissionProfileSelection {
            approval_policy: None,
            ..selection.clone()
        },
    )
    .await;
    app.settle_turn_submissions(&mut server).await?;
    let settings = next_thread_settings_updated(&mut server, thread_id).await;
    assert!(matches!(
        settings.thread_settings.sandbox_policy,
        codex_app_server_protocol::SandboxPolicy::ReadOnly { .. }
    ));
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::ThreadSettingsUpdated(settings),
    )
    .await?;
    app.chat_widget.handle_server_notification(
        turn_completed_notification(thread_id, "turn-1", TurnStatus::Completed),
        /*replay_kind*/ None,
    );
    // Unchanged re-selection is accepted without requiring a notification.
    app.select_permission_profile(&mut server, selection.clone())
        .await;
    app.settle_turn_submissions(&mut server).await?;
    assert!(!app.reject_pending_permission_change());
    assert!(app.agents_overview.requested_permission_profiles.is_empty());
    // Another client's policy change must not leave a no-op request's old policy behind.
    server
        .thread_settings_update(ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            approval_policy: Some(AskForApproval::OnRequest),
            ..Default::default()
        })
        .await?;
    let settings = next_thread_settings_updated(&mut server, thread_id).await;
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::ThreadSettingsUpdated(settings),
    )
    .await?;
    app.select_permission_profile(
        &mut server,
        PermissionProfileSelection {
            profile_id: "other".into(),
            approval_policy: None,
            approvals_reviewer: None,
            display_label: "other".into(),
        },
    )
    .await;
    app.settle_turn_submissions(&mut server).await?;
    assert_eq!(
        app.agents_overview.requested_permission_profiles[&thread_id].approval_policy,
        Some(AskForApproval::OnRequest)
    );
    let settings = next_thread_settings_updated(&mut server, thread_id).await;
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::ThreadSettingsUpdated(settings),
    )
    .await?;
    assert!(app.agents_overview.requested_permission_profiles.is_empty());
    let confirmed = RuntimePermissionProfileOverride::from_config(app.chat_widget.config_ref());
    let confirmed_config = app.config.clone();
    app.select_permission_profile(
        &mut server,
        PermissionProfileSelection {
            profile_id: "missing-on-server".into(),
            ..selection
        },
    )
    .await;
    app.settle_turn_submissions(&mut server).await?;
    assert_eq!(
        RuntimePermissionProfileOverride::from_config(app.chat_widget.config_ref()),
        confirmed
    );
    assert!(!app.pending_server_profiles.contains_key(&thread_id));
    assert_eq!(app.config, confirmed_config);
    assert_eq!(
        app.agents_overview
            .selected_permission_profiles
            .get(&thread_id)
            .map(String::as_str),
        Some("other")
    );
    // Materialize the task so /fork exercises persisted history with the confirmed profile.
    server.thread_inject_items(thread_id, vec![serde_json::from_value(serde_json::json!({
        "type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "saved history"}]
    }))?]).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while events.try_recv().is_ok() {}
    app.change_working_directory_and_settle(&mut tui, &mut server, client_home.path().abs())
        .await;
    assert_eq!(app.chat_widget.thread_id(), Some(thread_id));
    insta::assert_snapshot!(next_history_message(&mut events), @"■ Permission profile has different settings.");
    let side = Box::pin(
        server.fork_side_thread(
            &app.local_settings,
            app.config.clone(),
            thread_id,
            app.agents_overview
                .requested_permission_profiles
                .get(&thread_id),
            crate::app_server_session::SideChatStorage::Temporary,
        ),
    )
    .await?;
    assert_eq!(
        (
            side.session.permission_profile,
            side.session.approval_policy
        ),
        (PermissionProfile::read_only(), AskForApproval::OnRequest)
    );
    app.cli_kv_overrides.push((
        "permissions.other.extends".into(),
        TomlValue::String(":danger-full-access".into()),
    ));
    // Both the accepted request and its confirmed descendant remain server-owned.
    for _ in 0..2 {
        let previous_thread_id = app.chat_widget.thread_id();
        Box::pin(app.handle_event_and_settle_navigation(
            &mut tui,
            &mut server,
            AppEvent::ForkCurrentSession { name: None },
        ))
        .await?;
        assert_ne!(
            app.chat_widget.thread_id(),
            previous_thread_id,
            "{:?}",
            std::iter::from_fn(|| events.try_recv().ok())
                .filter_map(|event| match event {
                    AppEvent::InsertHistoryCell(cell) =>
                        Some(lines_to_single_string(&cell.display_lines(80))),
                    _ => None,
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(
            RuntimePermissionProfileOverride::from_config(app.chat_widget.config_ref()),
            confirmed
        );
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn accepted_permissions_survive_directory_change_before_notification() -> Result<()> {
    for (preserve_history, fork) in [(false, false), (true, false), (true, true)] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        app.config.ephemeral = false;
        app.config
            .permissions
            .set_permission_profile(PermissionProfile::Disabled)?;
        let destination = app.config.codex_home.join("destination");
        std::fs::create_dir(&destination)?;
        crate::legacy_core::config::set_project_trust_level(
            app.config.codex_home.as_path(),
            &destination,
            codex_protocol::config_types::TrustLevel::Trusted,
        )
        .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))?;
        let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let started = server.start_thread(&app.config).await?;
        let source = started.session.thread_id;
        app.enqueue_primary_thread_session(started.session, started.turns)
            .await?;
        app.runtime_permission_profile_override =
            Some(RuntimePermissionProfileOverride::from_config(&app.config));
        if preserve_history {
            server.thread_inject_items(source, vec![serde_json::from_value(serde_json::json!({
                "type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "saved history"}]
            }))?]).await?;
        }
        app.select_permission_profile(
            &mut server,
            PermissionProfileSelection {
                profile_id: ":read-only".into(),
                approval_policy: Some(AskForApproval::OnRequest),
                approvals_reviewer: Some(ApprovalsReviewer::User),
                display_label: "Read Only".into(),
            },
        )
        .await;
        if fork {
            // Fork admission must wait for permission acknowledgement before it
            // captures source authority, even without a settings notification.
            let mut tui = crate::tui::test_support::make_test_tui()?;
            app.fork_current_thread(&mut tui, &mut server, None, Vec::new(), false);
            assert_eq!(app.chat_widget.thread_id(), Some(source));
            app.settle_navigation(&mut tui, &mut server).await?;
            assert_ne!(app.chat_widget.thread_id(), Some(source));
            assert_eq!(
                app.chat_widget
                    .config_ref()
                    .permissions
                    .permission_profile(),
                &PermissionProfile::read_only()
            );
            assert_eq!(
                app.chat_widget.config_ref().approvals_reviewer,
                ApprovalsReviewer::User
            );
            server.shutdown().await?;
            continue;
        }
        if preserve_history {
            app.settle_turn_submissions(&mut server).await?;
            assert!(
                app.agents_overview
                    .requested_permission_profiles
                    .contains_key(&source),
                "selection must be unconfirmed"
            );
        }
        // Destination constraints belong to the client; the server's result is authoritative.
        let requirements = app.config.codex_home.join("client-requirements.toml");
        std::fs::write(&requirements, "allowed_approval_policies = [\"never\"]")?;
        app.loader_overrides.system_requirements_path = Some(requirements.to_path_buf());
        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.change_working_directory(&mut tui, &mut server, destination.clone())
            .await;
        if !preserve_history {
            // /cd must capture the profile after the preceding settings receipt.
            assert_eq!(app.chat_widget.thread_id(), Some(source));
            app.settle_turn_submissions(&mut server).await?;
            let admitted = std::iter::from_fn(|| events.try_recv().ok())
                .find(|event| matches!(event, AppEvent::DirectoryChangeAdmitted { .. }))
                .expect("directory change is released by its settings barrier");
            app.handle_event(&mut tui, &mut server, admitted).await?;
        }
        if preserve_history {
            // A confirmation can arrive while /cd is preparing its replacement. Retiring
            // the pending selection must not revoke the unchanged permission authority.
            let settings = next_thread_settings_updated(&mut server, source).await;
            app.enqueue_thread_notification(
                source,
                ServerNotification::ThreadSettingsUpdated(settings),
            )
            .await?;
            assert!(
                !app.agents_overview
                    .requested_permission_profiles
                    .contains_key(&source)
            );
        }
        app.settle_navigation(&mut tui, &mut server).await?;
        assert_ne!(
            app.chat_widget.thread_id(),
            Some(source),
            "preserve_history={preserve_history}: {}",
            std::iter::from_fn(|| events.try_recv().ok())
                .filter_map(|event| match event {
                    AppEvent::InsertHistoryCell(cell) =>
                        Some(lines_to_single_string(&cell.transcript_lines(200))),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert_eq!(
            (&app.config.cwd, app.config.permissions.permission_profile()),
            (&destination, &PermissionProfile::read_only())
        );
        assert_eq!(
            app.config.permissions.approval_policy.value(),
            AskForApproval::OnRequest.to_core()
        );
        assert_eq!(
            app.runtime_permission_profile_override,
            Some(RuntimePermissionProfileOverride::from_config(&app.config))
        );
        server.shutdown().await?;
    }
    Ok(())
}
