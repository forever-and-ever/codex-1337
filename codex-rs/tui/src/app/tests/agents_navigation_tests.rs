use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn agents_navigation_roster_tracks_background_metadata_without_replaying_a_child() {
    let mut app = make_test_app().await;
    let root = ThreadId::new();
    let child = ThreadId::new();
    app.enqueue_primary_thread_session(
        test_thread_session(root, app.config.cwd.to_path_buf()),
        Vec::new(),
    )
    .await
    .expect("primary session");
    app.upsert_agent_picker_thread(
        child,
        Some("worker".into()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let other = ThreadId::new();
    app.upsert_agent_picker_thread(
        other,
        Some("alpha".into()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let mut settings = codex_app_server_protocol::ThreadSettingsUpdatedNotification {
        thread_id: child.to_string(),
        thread_settings: codex_app_server_protocol::ThreadSettings {
            disabled_plugin_ids: Vec::new(),
            environments: None,
            cwd: app.config.cwd.clone(),
            approval_policy: AskForApproval::OnRequest,
            approvals_reviewer: codex_app_server_protocol::ApprovalsReviewer::AutoReview,
            sandbox_policy: codex_app_server_protocol::SandboxPolicy::ReadOnly {
                network_access: false,
            },
            active_permission_profile: None,
            model: "gpt-6.1-sol".into(),
            model_provider: "openai".into(),
            service_tier: None,
            effort: Some(ReasoningEffortConfig::XHigh),
            summary: None,
            collaboration_mode: app.chat_widget.current_collaboration_mode().clone(),
            multi_agent_mode: Default::default(),
            personality: None,
        },
    };
    app.track_subagent_panel(&ServerNotification::ThreadSettingsUpdated(settings.clone()));
    // A label-only navigation upsert must preserve the server's model selection.
    app.upsert_agent_picker_thread(
        child,
        Some("worker".into()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let status = |status| {
        ServerNotification::ThreadStatusChanged(
            codex_app_server_protocol::ThreadStatusChangedNotification {
                thread_id: child.to_string(),
                status,
            },
        )
    };
    app.track_subagent_panel(&status(codex_app_server_protocol::ThreadStatus::Active {
        active_flags: Vec::new(),
    }));
    let activity = ServerNotification::ThreadWorkStatusUpdated(
        codex_app_server_protocol::ThreadWorkStatusUpdatedNotification {
            thread_id: child.to_string(),
            status: codex_protocol::protocol::WorkStatus {
                turn_id: "turn".into(),
                activity: "Inspect parser".into(),
                emitted_at: None,
            },
        },
    );
    app.track_subagent_panel(&activity);
    let mut usage = token_usage_notification(child, "turn", /*model_context_window*/ None);
    if let ServerNotification::ThreadTokenUsageUpdated(event) = &mut usage {
        // Match the status line: session totals, excluding cached input, with
        // reasoning already included in output (not counted a second time).
        event.token_usage.total = TokenUsageBreakdown {
            cost: None,
            total_tokens: 20_000_000,
            input_tokens: 19_000_000,
            cached_input_tokens: 18_500_000,
            cache_write_input_tokens: 0,
            output_tokens: 1_000_000,
            reasoning_output_tokens: 200_000,
        };
    }
    app.track_subagent_panel(&usage);
    {
        let roster = app.agent_navigation.roster.lock().unwrap();
        assert_eq!(
            roster.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![child, other]
        );
        let row = &roster.rows[0];
        assert_eq!(
            (
                row.activity.as_deref(),
                row.tokens,
                roster.watched,
                roster.focused,
                row.model.clone(),
            ),
            (
                Some("Inspect parser"),
                Some(1_500_000),
                Some(root),
                None,
                Some(crate::subagent_roster::AgentModel {
                    model: "gpt-6.1-sol".into(),
                    effort: Some(ReasoningEffortConfig::XHigh),
                }),
            )
        );
    }
    app.track_subagent_panel(&status(codex_app_server_protocol::ThreadStatus::Idle));
    settings.thread_settings.model = "claude-sonnet-4-6".into();
    settings.thread_settings.effort = None;
    app.track_subagent_panel(&ServerNotification::ThreadSettingsUpdated(settings));
    app.track_subagent_panel(&activity);
    let roster = app.agent_navigation.roster.lock().unwrap();
    assert_eq!(
        roster.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![other, child]
    );
    let row = &roster.rows[1];
    assert_eq!(
        (&row.status, &row.activity, row.tokens),
        (
            &Some(codex_app_server_protocol::ThreadStatus::Idle),
            &None,
            Some(1_500_000)
        )
    );
    drop(roster);

    // Unloading hides the inline row without losing the recoverable picker entry.
    app.agent_navigation.remove(other);
    app.sync_subagent_roster();
    app.agent_navigation.roster.lock().unwrap().down();
    app.track_subagent_panel(&status(codex_app_server_protocol::ThreadStatus::NotLoaded));
    app.sync_subagent_roster();
    {
        let roster = app.agent_navigation.roster.lock().unwrap();
        assert_eq!((roster.rows.len(), roster.focused), (0, None));
    }
    assert!(app.agent_navigation.get(&child).unwrap().is_closed);
    insta::assert_snapshot!(
        "subagent_roster_unloaded",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );
    app.track_subagent_panel(&status(codex_app_server_protocol::ThreadStatus::Idle));
    let roster = app.agent_navigation.roster.lock().unwrap();
    assert_eq!(
        roster
            .rows
            .iter()
            .map(|row| (row.id, row.status.clone(), row.model.clone()))
            .collect::<Vec<_>>(),
        vec![(
            child,
            Some(codex_app_server_protocol::ThreadStatus::Idle),
            Some(crate::subagent_roster::AgentModel {
                model: "claude-sonnet-4-6".into(),
                effort: None,
            }),
        )]
    );
    drop(roster);
    app.agent_navigation.roster.lock().unwrap().down();
    app.agent_navigation.roster.lock().unwrap().retiring =
        Some(crate::subagent_roster::Retirement::Pending);
    insta::assert_snapshot!(
        "subagent_roster_retiring",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );
    app.agent_navigation.roster.lock().unwrap().retiring = None;
    app.agent_navigation.retire(child);
    // A response issued before retirement cannot resurrect the retired row.
    app.upsert_agent_picker_thread(
        child,
        Some("worker".into()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.sync_subagent_roster();
    assert!(app.agent_navigation.roster.lock().unwrap().rows.is_empty());
}

#[tokio::test]
async fn agents_navigation_picker_keeps_selection_when_running_order_changes() {
    let mut app = make_test_app().await;
    let root = ThreadId::new();
    let idle = ThreadId::new();
    let busy = ThreadId::new();
    let unloaded = ThreadId::new();
    app.primary_thread_id = Some(root);
    app.active_thread_id = Some(idle);
    app.upsert_agent_picker_thread(
        unloaded,
        Some("unloaded".into()),
        /*agent_role*/ None,
        /*is_closed*/ true,
    );
    for (id, name) in [(root, "main"), (idle, "researcher"), (busy, "reviewer")] {
        app.upsert_agent_picker_thread(
            id,
            Some(name.into()),
            /*agent_role*/ None,
            /*is_closed*/ false,
        );
    }
    for id in [root, idle] {
        app.agent_navigation
            .set_status(id, codex_app_server_protocol::ThreadStatus::Idle);
    }
    app.agent_navigation.set_status(
        busy,
        codex_app_server_protocol::ThreadStatus::Active {
            active_flags: Vec::new(),
        },
    );
    let first = app.agent_picker_selection_view_params(/*selected*/ None);
    assert_eq!(
        app.agent_navigation.picker_row_ids,
        vec![busy, root, idle, unloaded]
    );
    assert_eq!(first.initial_selected_idx, Some(2));
    app.agent_navigation
        .set_status(busy, codex_app_server_protocol::ThreadStatus::Idle);
    app.agent_navigation.set_status(
        idle,
        codex_app_server_protocol::ThreadStatus::Active {
            active_flags: Vec::new(),
        },
    );
    let next = app.agent_picker_selection_view_params(Some(idle));
    assert_eq!(
        app.agent_navigation.picker_row_ids,
        vec![idle, root, busy, unloaded]
    );
    assert_eq!(next.initial_selected_idx, Some(0));
    app.chat_widget.show_selection_view(next);
    insta::assert_snapshot!(
        "subagents_running_first",
        render_bottom_popup(&app.chat_widget, /*width*/ 90)
            .replace(&root.to_string(), "ROOT_ID")
            .replace(&idle.to_string(), "IDLE_ID")
            .replace(&busy.to_string(), "BUSY_ID")
            .replace(&unloaded.to_string(), "UNLOADED_ID")
    );
}

#[tokio::test]
async fn agents_navigation_escape_returns_to_main_but_preserves_drafts_and_popups() -> Result<()> {
    let (mut app, mut events, _operations) = make_test_app_with_channels().await;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let root = app_server.start_thread(&app.config).await?;
    let root_id = root.session.thread_id;
    app.enqueue_primary_thread_session(root.session, root.turns)
        .await?;
    let child = app_server.start_thread(&app.config).await?;
    let child_id = child.session.thread_id;
    app.thread_event_channels.insert(
        child_id,
        ThreadEventChannel::new_with_session(/*capacity*/ 1, child.session, child.turns),
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.select_agent_thread_and_settle(&mut tui, &mut app_server, child_id)
        .await?;
    assert_eq!(app.current_displayed_thread_id(), Some(child_id));
    app.chat_widget.apply_external_edit("x".into());
    assert_eq!(app.chat_widget.composer_text_with_pending(), "x");
    app.handle_key_event(&mut tui, &mut app_server, KeyCode::Esc.into())
        .await;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert_eq!(app.current_displayed_thread_id(), Some(child_id));
    app.chat_widget.apply_external_edit(String::new());
    assert!(app.chat_widget.composer_is_empty());
    app.chat_widget.show_selection_view(SelectionViewParams {
        title: Some("Child popup".into()),
        items: vec![SelectionItem {
            name: "Keep working".into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    app.handle_key_event(&mut tui, &mut app_server, KeyCode::Esc.into())
        .await;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert_eq!(app.current_displayed_thread_id(), Some(child_id));
    assert!(app.chat_widget.no_modal_or_popup_active());
    app.handle_key_event(&mut tui, &mut app_server, KeyCode::Esc.into())
        .await;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert_eq!(app.current_displayed_thread_id(), Some(root_id));
    app.upsert_agent_picker_thread(
        child_id,
        Some("child".into()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.agent_navigation.mark_parent_owned(child_id);
    // The inline roster must not swallow the first character or leave arrow keys
    // navigating agents after typing has resumed in the composer.
    for (character, modifiers) in [
        ('x', KeyModifiers::NONE),
        ('Ж', KeyModifiers::SHIFT),
        (' ', KeyModifiers::NONE),
    ] {
        app.chat_widget.apply_external_edit(String::new());
        app.handle_key_event(&mut tui, &mut app_server, KeyCode::Down.into())
            .await;
        app.settle_navigation(&mut tui, &mut app_server).await?;
        assert_eq!(
            app.agent_navigation.roster.lock().unwrap().focused,
            Some(child_id)
        );
        app.handle_key_event(
            &mut tui,
            &mut app_server,
            KeyEvent::new(KeyCode::Char(character), modifiers),
        )
        .await;
        app.settle_navigation(&mut tui, &mut app_server).await?;
        std::thread::sleep(crate::bottom_pane::ChatComposer::recommended_paste_flush_delay());
        app.chat_widget
            .handle_paste_burst_tick(tui.frame_requester());
        assert_eq!(
            (
                app.agent_navigation.roster.lock().unwrap().focused,
                app.chat_widget.composer_text_with_pending(),
            ),
            (None, character.to_string())
        );
    }
    app.chat_widget
        .apply_external_edit("Preserve this root draft".into());
    app.handle_key_event(&mut tui, &mut app_server, KeyCode::Down.into())
        .await;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert_eq!(
        app.agent_navigation.roster.lock().unwrap().focused,
        Some(child_id)
    );
    app.handle_key_event(&mut tui, &mut app_server, KeyCode::Enter.into())
        .await;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert_eq!(app.current_displayed_thread_id(), Some(child_id));
    app.chat_widget
        .apply_external_edit("Hidden child draft".into());
    app.chat_widget.set_parent_owned_thread();
    app.handle_key_event(&mut tui, &mut app_server, KeyCode::Up.into())
        .await;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "Hidden child draft"
    );
    app.handle_key_event(&mut tui, &mut app_server, KeyCode::Esc.into())
        .await;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert_eq!(app.current_displayed_thread_id(), Some(child_id));
    app.chat_widget.set_child_text_input_enabled();
    tui.set_owned_screen(/*owned*/ true)?;
    // Parent-owned children still have a composer: clearing and recovering its
    // draft must win over interruption, even while both child and root are busy.
    app.enqueue_thread_notification(child_id, turn_started_notification(child_id, "child-turn"))
        .await?;
    app.drain_active_thread_events(&mut tui).await?;
    let root_store = Arc::clone(&app.ensure_thread_channel(root_id).store);
    root_store
        .lock()
        .await
        .set_active_turn_id("root-turn".into());
    let child_store = Arc::clone(&app.ensure_thread_channel(child_id).store);
    while events.try_recv().is_ok() {}
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    app.transcript_view.begin_search();
    app.handle_tui_event(&mut tui, &mut app_server, TuiEvent::Key(ctrl_c))
        .await?;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert!(!app.transcript_view.is_search_editing());
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "Hidden child draft"
    );
    app.handle_tui_event(&mut tui, &mut app_server, TuiEvent::Key(ctrl_c))
        .await?;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    let child_pending_interrupt = child_store.lock().await.pending_interrupt_turn_id.clone();
    let root_pending_interrupt = root_store.lock().await.pending_interrupt_turn_id.clone();
    assert_eq!(
        (
            app.chat_widget.composer_text_with_pending(),
            app.chat_widget.is_task_running_for_test(),
            child_pending_interrupt,
            root_pending_interrupt,
        ),
        (String::new(), true, None, None)
    );
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::CodexOp(_) | AppEvent::Exit(_)))
    );
    app.handle_key_event(&mut tui, &mut app_server, KeyCode::Up.into())
        .await;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "Hidden child draft"
    );
    app.handle_tui_event(&mut tui, &mut app_server, TuiEvent::Key(ctrl_c))
        .await?;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert!(app.chat_widget.composer_is_empty());
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::CodexOp(_) | AppEvent::Exit(_)))
    );
    app.handle_tui_event(&mut tui, &mut app_server, TuiEvent::Key(ctrl_c))
        .await?;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    let interrupt = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| matches!(event, AppEvent::CodexOp(_) | AppEvent::Exit(_)))
        .expect("empty child composer should interrupt");
    assert!(matches!(
        interrupt,
        AppEvent::CodexOp(AppCommand::Interrupt)
    ));
    Box::pin(app.handle_event(&mut tui, &mut app_server, interrupt)).await?;
    let child_pending_interrupt = child_store.lock().await.pending_interrupt_turn_id.clone();
    let root_pending_interrupt = root_store.lock().await.pending_interrupt_turn_id.clone();
    assert_eq!(
        (child_pending_interrupt, root_pending_interrupt),
        (Some("child-turn".into()), None)
    );
    root_store.lock().await.clear_active_turn_id();
    app.enqueue_thread_notification(
        child_id,
        turn_completed_notification(child_id, "child-turn", TurnStatus::Interrupted),
    )
    .await?;
    app.drain_active_thread_events(&mut tui).await?;
    while events.try_recv().is_ok() {}
    app.handle_tui_event(&mut tui, &mut app_server, TuiEvent::Key(ctrl_c))
        .await?;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::Exit(ExitMode::ShutdownFirst)))
    );
    app.handle_key_event(&mut tui, &mut app_server, KeyCode::Esc.into())
        .await;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    assert_eq!(app.current_displayed_thread_id(), Some(root_id));
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "Preserve this root draft"
    );
    // The owned transcript routes input before handle_key_event. An idle child
    // previously consumed Escape as prompt backtracking instead of navigation.
    for phase in ["running", "finished", "stopped", "finishing"] {
        app.select_agent_thread_and_settle(&mut tui, &mut app_server, child_id)
            .await?;
        app.chat_widget.apply_external_edit(String::new());
        app.enqueue_thread_notification(child_id, turn_started_notification(child_id, phase))
            .await?;
        app.drain_active_thread_events(&mut tui).await?;
        assert!(app.chat_widget.is_task_running_for_test());
        if phase != "running" {
            let status = if phase == "stopped" {
                TurnStatus::Interrupted
            } else {
                TurnStatus::Completed
            };
            app.enqueue_thread_notification(
                child_id,
                turn_completed_notification(child_id, phase, status),
            )
            .await?;
            if phase != "finishing" {
                app.drain_active_thread_events(&mut tui).await?;
                assert!(!app.chat_widget.is_task_running_for_test());
            }
        }
        app.transcript_view.begin_search();
        app.handle_tui_event(
            &mut tui,
            &mut app_server,
            TuiEvent::Key(KeyCode::Esc.into()),
        )
        .await?;
        app.settle_navigation(&mut tui, &mut app_server).await?;
        assert_eq!(app.current_displayed_thread_id(), Some(child_id));
        assert!(!app.transcript_view.is_search_editing());
        app.handle_tui_event(
            &mut tui,
            &mut app_server,
            TuiEvent::Key(KeyCode::Esc.into()),
        )
        .await?;
        app.settle_navigation(&mut tui, &mut app_server).await?;
        assert_eq!(
            (
                app.current_displayed_thread_id(),
                app.backtrack.primed,
                app.backtrack.overlay_preview_active,
                app.chat_widget.composer_text_with_pending(),
            ),
            (
                Some(root_id),
                false,
                false,
                "Preserve this root draft".into()
            ),
            "{phase}"
        );
    }
    insta::assert_snapshot!(
        "subagent_escape_restored_root",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );
    tui.set_owned_screen(/*owned*/ false)?;
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn agents_navigation_requires_shared_app_server() -> Result<()> {
    let endpoint = crate::RemoteAppServerEndpoint::UnixSocket {
        socket_path: AbsolutePathBuf::relative_to_current_dir("codex.sock")?,
    };
    for target in [
        AppServerTarget::Embedded,
        AppServerTarget::Remote {
            endpoint: endpoint.clone(),
        },
        AppServerTarget::LocalDaemon {
            endpoint,
            allow_embedded_fallback: true,
        },
    ] {
        let (mut app, mut events, _op_rx) = make_test_app_with_channels().await;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        let mut app_server = start_config_write_test_app_server(&app).await?;
        let shared = !matches!(target, AppServerTarget::Embedded);
        app.app_server_target = target;
        let init = app.chatwidget_init_for_forked_or_resumed_thread(
            &mut tui,
            app.config.clone(),
            /*initial_user_message*/ None,
        );
        app.replace_chat_widget(ChatWidget::new_with_app_event(init));
        while events.try_recv().is_ok() {}
        let composer = render_bottom_popup(&app.chat_widget, /*width*/ 96);
        let footer = composer.lines().last().expect("composer footer").trim();
        insta::allow_duplicates! {
            insta::assert_snapshot!(normalize_snapshot_paths(footer), @"← for agents · ? for shortcuts");
        }
        app.handle_tui_event(
            &mut tui,
            &mut app_server,
            TuiEvent::Key(KeyCode::Left.into()),
        )
        .await?;
        let event = events.try_recv()?;
        assert_matches!(event, AppEvent::OpenAgentsOverview);
        app.handle_event(&mut tui, &mut app_server, event).await?;
        assert_eq!(
            app.chat_widget.active_view_is(AGENTS_OVERVIEW_VIEW_ID),
            shared
        );
        if !shared {
            assert!(
                render_bottom_popup(&app.chat_widget, 96).contains("Shared agents unavailable")
            );
            assert!(render_bottom_popup(&app.chat_widget, 96).contains("Return to this session"));
        }
        app_server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn externally_archived_thread_keeps_advertised_agents_navigation() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    app.app_server_target = AppServerTarget::LocalDaemon {
        endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
            socket_path: AbsolutePathBuf::relative_to_current_dir("codex.sock")?,
        },
        allow_embedded_fallback: true,
    };
    let init = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui,
        app.config.clone(),
        /*initial_user_message*/ None,
    );
    app.replace_chat_widget(ChatWidget::new_with_app_event(init));
    let id = ThreadId::new();
    app.active_thread_id = Some(id);
    app.primary_thread_id = Some(id);
    app.chat_widget
        .handle_thread_session(test_thread_session(id, app.config.cwd.to_path_buf()));
    app.ensure_thread_channel(id);
    app.handle_app_server_event(
        &app_server,
        codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
            ServerNotification::ThreadArchived(ThreadArchivedNotification {
                thread_id: id.to_string(),
            }),
        )),
    )
    .await;
    while events.try_recv().is_ok() {}

    // Left must still edit an existing draft before it can navigate from an empty composer.
    app.chat_widget
        .restore_user_message_to_composer("ab".into());
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        TuiEvent::Key(KeyCode::Left.into()),
    )
    .await?;
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        TuiEvent::Key(KeyCode::Char('!').into()),
    )
    .await?;
    assert_eq!(app.chat_widget.composer_text_with_pending(), "a!b");
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::OpenAgentsOverview))
    );
    for key in [
        KeyCode::End.into(),
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    ] {
        app.handle_tui_event(&mut tui, &mut app_server, TuiEvent::Key(key))
            .await?;
    }
    assert!(app.chat_widget.composer_is_empty());
    assert!(render_bottom_popup(&app.chat_widget, /*width*/ 96).contains("for agents"));
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        TuiEvent::Key(KeyEvent::new_with_kind(
            KeyCode::Left,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )),
    )
    .await?;
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::OpenAgentsOverview))
    );
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        TuiEvent::Key(KeyCode::Left.into()),
    )
    .await?;
    let event = events.try_recv()?;
    assert_matches!(event, AppEvent::OpenAgentsOverview);
    app.handle_event(&mut tui, &mut app_server, event).await?;
    insta::assert_snapshot!(
        "externally_archived_thread_agents",
        render_bottom_popup(&app.chat_widget, /*width*/ 96)
    );
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn pending_windows_sandbox_setup_blocks_thread_replacement() -> Result<()> {
    use crate::app_event::WindowsSandboxEnableMode;

    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.chat_widget.windows_sandbox_local_server = true;
    while events.try_recv().is_ok() {}
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let current = ThreadId::new();
    let other = ThreadId::new();
    app.active_thread_id = Some(current);
    app.primary_thread_id = Some(current);
    let preset = codex_utils_approval_presets::builtin_approval_presets()
        .into_iter()
        .find(|preset| preset.id == "auto")
        .expect("auto preset");
    app.windows_sandbox.pending_setup = Some((WindowsSandboxEnableMode::Elevated, preset, None));
    let prompt = crate::chatwidget::create_initial_user_message(
        Some("review this".to_string()),
        Vec::new(),
        Vec::new(),
    );
    app.chat_widget.initial_user_message = prompt.clone();

    app.select_agent_thread_and_settle(&mut tui, &mut app_server, other)
        .await?;
    assert_eq!(app.active_thread_id, Some(current));
    assert_eq!(app.chat_widget.initial_user_message, prompt);
    let cell = match events.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected setup navigation message, got {other:?}"),
    };
    let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 100));
    insta::assert_snapshot!(rendered, @"• Finish Windows sandbox setup before switching threads.");

    app.chat_widget.initial_user_message = None;
    app.run_agents_overview_action(
        &mut tui,
        &mut app_server,
        current,
        crate::app_event::AgentsOverviewAction::Archive,
    )
    .await?;
    assert_eq!(app.active_thread_id, Some(current));
    assert!(events.try_recv().is_err());

    app_server.shutdown().await?;
    Ok(())
}
