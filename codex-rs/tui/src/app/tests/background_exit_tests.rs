use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn external_writer_view_quits_with_ctrl_c_or_q() -> Result<()> {
    let (mut app, mut events, _operations) = make_test_app_with_channels().await;
    app.chat_widget.show_external_writer_thread();
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while events.try_recv().is_ok() {}

    app.keymap.app.open_external_editor = vec![crate::key_hint::ctrl(KeyCode::Char('g'))];
    app.handle_key_event(
        &mut tui,
        &mut app_server,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
    )
    .await;
    assert_eq!(
        app.chat_widget.external_editor_state(),
        ExternalEditorState::Closed
    );
    assert!(
        !events
            .try_recv()
            .is_ok_and(|event| matches!(event, AppEvent::LaunchExternalEditor))
    );

    for key in [
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    ] {
        app.handle_key_event(&mut tui, &mut app_server, key).await;
        assert!(matches!(
            events.try_recv(),
            Ok(AppEvent::Exit(ExitMode::Immediate))
        ));
    }
    Ok(())
}

#[tokio::test]
async fn embedded_quit_confirms_stopping_a_running_side_conversation() -> Result<()> {
    use core_test_support::responses;
    use core_test_support::streaming_sse::StreamingSseChunk;
    use core_test_support::streaming_sse::start_streaming_sse_server;

    let (release, gate) = tokio::sync::oneshot::channel();
    let (model, _) = start_streaming_sse_server(vec![vec![
        StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_response_created("running-response"),
                responses::ev_message_item_added("running-message", "Working"),
            ]),
        },
        StreamingSseChunk {
            gate: Some(gate),
            body: responses::sse(vec![
                responses::ev_assistant_message("running-message", "Finished"),
                responses::ev_completed("running-response"),
            ]),
        },
    ]])
    .await;
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    app_test_support::MockResponsesConfig::new(model.uri()).write(&app.config.codex_home)?;
    app.config = app
        .rebuild_config_for_cwd(app.config.cwd.to_path_buf())
        .await?;
    let (mut server, mut tui) = prepare_background_exit_test(&app, &mut events, &mut ops).await?;
    let started = server.start_thread(&app.config).await?;
    let id = started.session.thread_id;
    let request_id = server.next_request_id();
    let running: codex_app_server_protocol::TurnStartResponse = server
        .request_handle()
        .request_typed(codex_app_server_protocol::ClientRequest::TurnStart {
            request_id,
            params: codex_app_server_protocol::TurnStartParams {
                thread_id: id.to_string(),
                input: vec![codex_app_server_protocol::UserInput::Text {
                    text: "Keep running until stopped.".into(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = server.next_event().await.expect("live side turn");
            if matches!(&event.event, codex_app_server_client::AppServerEvent::ServerNotification(notification)
                if matches!(notification.as_ref(), ServerNotification::ItemStarted(item) if item.item.payload.id() == "running-message"))
            { break; }
        }
    }).await?;
    assert_eq!(running.turn.status, TurnStatus::InProgress);
    app.side_threads
        .insert(id, SideThreadState::new(ThreadId::new()));
    app.thread_event_channels.insert(
        id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            started.session,
            vec![running.turn],
        ),
    );
    assert_matches!(
        app.handle_event(&mut tui, &mut server, AppEvent::Exit(ExitMode::Immediate))
            .await?,
        AppRunControl::Continue
    );
    insta::assert_snapshot!(
        "embedded_exit_running_sessions",
        render_bottom_popup(&app.chat_widget, /*width*/ 90)
    );
    app.chat_widget.handle_key_event(KeyCode::Down.into());
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    let stop = events.try_recv()?;
    assert_matches!(&stop, AppEvent::Exit(ExitMode::ConfirmedStop));
    assert_matches!(
        app.handle_event(&mut tui, &mut server, stop).await?,
        AppRunControl::Continue
    );
    assert!(
        app.thread_event_channels.contains_key(&id),
        "Stop admission is not a cleanup ACK"
    );
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let outcome = tokio::select! {
                completion = app.server_operations.control_rx.recv() => {
                    Box::pin(app.handle_server_completion(&mut tui, &mut server,
                        completion.expect("owned Stop cleanup receipt"))).await?
                }
                event = events.recv() => {
                    Box::pin(app.handle_event(&mut tui, &mut server,
                        event.expect("owned Stop exit continuation"))).await?
                }
            };
            if matches!(outcome, AppRunControl::Exit(_)) {
                break Ok::<_, color_eyre::Report>(outcome);
            }
        }
    })
    .await??;
    assert_matches!(outcome, AppRunControl::Exit(ExitReason::UserRequested));
    assert!(!app.thread_event_channels.contains_key(&id));
    let _ = release.send(());
    server.shutdown().await?;
    model.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn external_writer_view_preserves_draft_from_keys_and_paste() -> Result<()> {
    let (mut app, _events, _operations) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.enqueue_primary_thread_session(
        test_thread_session(thread_id, test_path_buf("/tmp/project")),
        Vec::new(),
    )
    .await?;
    app.app_server_target = AppServerTarget::Remote {
        endpoint: crate::RemoteAppServerEndpoint::WebSocket {
            websocket_url: "wss://example.com/".to_string(),
            auth_token: None,
        },
    };
    app.ensure_thread_channel(thread_id).mark_external_writer();
    app.transcript_cells
        .push(Arc::new(history_cell::new_user_prompt(
            "Existing prompt".to_string(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )));
    app.chat_widget.insert_str("Retained draft");
    app.chat_widget.show_external_writer_thread();
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    for offline in [false, true] {
        app.reconnect.offline = offline;
        app.handle_tui_event(
            &mut tui,
            &mut app_server,
            TuiEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        )
        .await?;
        app.handle_tui_event(&mut tui, &mut app_server, TuiEvent::Paste("pasted".into()))
            .await?;
        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            "Retained draft"
        );
    }

    app.reconnect.offline = false;
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
    )
    .await?;
    assert!(app.overlay.is_some());
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        TuiEvent::Key(KeyCode::Esc.into()),
    )
    .await?;
    assert!(app.overlay.is_none());
    assert_eq!(
        (
            app.backtrack.primed,
            app.backtrack.overlay_preview_active,
            app.chat_widget.composer_text_with_pending(),
        ),
        (false, false, "Retained draft".to_string()),
    );
    Ok(())
}

#[tokio::test]
async fn external_writer_view_opens_agents_with_left() -> Result<()> {
    for offline in [false, true] {
        let (mut app, _, _) = make_test_app_with_channels().await;
        app.app_server_target = AppServerTarget::Remote {
            endpoint: crate::resolve_remote_addr("ws://127.0.0.1:4500")?,
        };
        app.chat_widget.insert_str("Retained draft");
        app.chat_widget.show_external_writer_thread();
        app.reconnect.offline = offline;
        let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        tui.set_owned_screen(/*owned*/ true)?;

        app.handle_tui_event(
            &mut tui,
            &mut app_server,
            TuiEvent::Key(KeyCode::Left.into()),
        )
        .await?;

        assert!(
            app.chat_widget
                .selected_index_for_present_view(AGENTS_OVERVIEW_VIEW_ID)
                .is_some()
        );
        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            "Retained draft"
        );
        tui.set_owned_screen(/*owned*/ false)?;
        app_server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn external_writer_view_respects_remapped_left() -> Result<()> {
    let config: codex_config::types::TuiKeymap = toml::from_str(
        "[global]\nopen_transcript = 'left'\n[editor]\nmove_left = 'ctrl-b'\n[list]\nmove_left = 'ctrl-h'\n",
    )?;
    let keymap = RuntimeKeymap::from_config(&config).map_err(|err| color_eyre::eyre::eyre!(err))?;
    for (offline, vim) in [(false, false), (true, false), (false, true), (true, true)] {
        let (mut app, _, _) = make_test_app_with_channels().await;
        app.app_server_target = AppServerTarget::Remote {
            endpoint: crate::resolve_remote_addr("ws://127.0.0.1:4500")?,
        };
        app.chat_widget.apply_keymap_update(config.clone(), &keymap);
        app.keymap = keymap.clone();
        if vim {
            app.chat_widget.toggle_vim_mode_and_notify();
        }
        app.chat_widget.show_external_writer_thread();
        app.reconnect.offline = offline;
        let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let mut tui = crate::tui::test_support::make_test_tui()?;

        if !offline && vim {
            insta::assert_snapshot!(render_bottom_popup(&app.chat_widget, /*width*/ 96));
        }
        app.handle_tui_event(
            &mut tui,
            &mut app_server,
            TuiEvent::Key(KeyCode::Left.into()),
        )
        .await?;
        assert!(app.chat_widget.no_modal_or_popup_active());
        assert_eq!(app.overlay.is_some(), !offline);
        app_server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn daemon_disconnect_exit_summary_includes_reconnect_and_stop_instructions() -> Result<()> {
    let (mut app, _, _) = make_test_app_with_channels().await;
    let thread_id = prepare_local_daemon_thread(&mut app)?;
    // Allocating a task is not saving it: the empty task's ID cannot be resumed.
    assert_snapshot!(
        app.exit_info(ExitReason::UserRequested)
            .format_exit_messages(/*color_enabled*/ false)
            .join("\n"),
        @""
    );
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "turn-1"),
        /*replay_kind*/ None,
    );
    app.keymap.agents.stop = vec![crate::key_hint::plain(KeyCode::F(10))];
    let mut exit_info = app.exit_info(ExitReason::UserRequested);
    exit_info.token_usage = TokenUsage {
        input_tokens: 10,
        output_tokens: 2,
        total_tokens: 12,
        ..Default::default()
    };
    let output = exit_info
        .format_exit_messages(/*color_enabled*/ false)
        .join("\n")
        .replace(&thread_id.to_string(), "THREAD_ID");
    assert_snapshot!("daemon_disconnect_exit", output);
    Ok(())
}

#[tokio::test]
async fn remote_disconnect_exit_summary_does_not_require_a_local_rollout_or_print_credentials() {
    let (mut app, _, _) = make_test_app_with_channels().await;
    app.app_server_target = AppServerTarget::Remote {
        endpoint: crate::RemoteAppServerEndpoint::WebSocket {
            websocket_url: "wss://user:secret@example.com:443/?token=private#secret".to_string(),
            auth_token: Some("secret-token".to_string()),
        },
    };
    let thread_id = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();
    app.active_thread_id = Some(thread_id);
    app.chat_widget.handle_thread_session(test_thread_session(
        thread_id,
        test_path_buf("/tmp/project"),
    ));
    let exit_info = app.exit_info(ExitReason::Fatal("connection lost".to_string()));
    let lines = exit_info.format_exit_messages(/*color_enabled*/ false);
    let command = shlex::split(&lines[2]).unwrap();
    assert_eq!(
        crate::resolve_remote_addr(&command[2]).unwrap(),
        crate::RemoteAppServerEndpoint::WebSocket {
            websocket_url: "wss://example.com/".to_string(),
            auth_token: None,
        }
    );
    assert_snapshot!("remote_disconnect_exit", lines.join("\n"));
}

#[tokio::test]
async fn embedded_exit_keeps_the_session_summary() {
    let (mut app, _, _) = make_test_app_with_channels().await;
    let thread_id = prepare_local_daemon_thread(&mut app).unwrap();
    app.app_server_target = AppServerTarget::Embedded;
    let mut exit_info = app.exit_info(ExitReason::UserRequested);
    exit_info.token_usage = TokenUsage {
        output_tokens: 2,
        total_tokens: 2,
        ..Default::default()
    };
    exit_info.resume_hint = Some(ResumableThread {
        thread_id,
        thread_name: None,
    });
    let output = exit_info
        .format_exit_messages(/*color_enabled*/ false)
        .join("\n")
        .replace(&thread_id.to_string(), "THREAD_ID");
    assert_snapshot!(output, @"
    Token usage: total=2 input=0 output=2
    To continue this session, run:
      codex resume THREAD_ID
    ");
}

fn prepare_local_daemon_thread(app: &mut App) -> Result<ThreadId> {
    app.app_server_target = AppServerTarget::LocalDaemon {
        allow_embedded_fallback: true,
        endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
            socket_path: AbsolutePathBuf::relative_to_current_dir("codex.sock")?,
        },
    };
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);
    app.chat_widget.handle_thread_session(test_thread_session(
        thread_id,
        test_path_buf("/tmp/project"),
    ));
    Ok(thread_id)
}

fn prepare_running_local_daemon(app: &mut App) -> Result<ThreadId> {
    let thread_id = prepare_local_daemon_thread(app)?;
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "turn-1"),
        /*replay_kind*/ None,
    );
    Ok(thread_id)
}

async fn open_running_task_exit_menu(
    app: &mut App,
    tui: &mut tui::Tui,
    app_server: &mut AppServerSession,
) {
    app.handle_key_event(
        tui,
        app_server,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await;
}

async fn prepare_background_exit_test(
    app: &App,
    app_event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
    op_rx: &mut mpsc::UnboundedReceiver<Op>,
) -> Result<(AppServerSession, tui::Tui)> {
    while app_event_rx.try_recv().is_ok() {}
    while op_rx.try_recv().is_ok() {}
    let app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    Ok((app_server, crate::tui::test_support::make_test_tui()?))
}

#[tokio::test]
async fn daemon_ctrl_c_shows_background_exit_menu_and_escape_dismisses_it() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    prepare_running_local_daemon(&mut app)?;
    let (mut app_server, mut tui) =
        prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;

    open_running_task_exit_menu(&mut app, &mut tui, &mut app_server).await;

    assert!(!app.chat_widget.no_modal_or_popup_active());
    assert_snapshot!(render_bottom_popup(&app.chat_widget, /*width*/ 90), @r"
      Task is still running
      Choose what happens to the current task.


    › 1. Cancel task        Stop the current task and stay in Codex
      2. Run in background  Exit Codex and leave the task running
      3. Exit               Stop the current task and exit Codex

      enter select · esc back
    ");

    app.handle_key_event(
        &mut tui,
        &mut app_server,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    )
    .await;

    assert!(app.chat_widget.no_modal_or_popup_active());
    assert!(op_rx.try_recv().is_err());
    assert!(app_event_rx.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn cancel_task_interrupts_without_exiting() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    prepare_running_local_daemon(&mut app)?;
    let (mut app_server, mut tui) =
        prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;
    open_running_task_exit_menu(&mut app, &mut tui, &mut app_server).await;

    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let event = app_event_rx.try_recv()?;
    assert_matches!(
        event,
        AppEvent::RunningTaskExit {
            action: RunningTaskExitAction::CancelTask,
            ..
        }
    );
    let control = app.handle_event(&mut tui, &mut app_server, event).await?;

    assert_matches!(control, AppRunControl::Continue);
    assert_matches!(op_rx.try_recv(), Ok(Op::Interrupt));
    assert!(app_event_rx.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn run_in_background_detaches_without_interrupting_main_or_side_threads() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let parent_thread_id = prepare_running_local_daemon(&mut app)?;
    let side_thread_id = ThreadId::new();
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));
    let (mut app_server, mut tui) =
        prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;
    open_running_task_exit_menu(&mut app, &mut tui, &mut app_server).await;

    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let event = app_event_rx.try_recv()?;
    assert_matches!(
        event,
        AppEvent::RunningTaskExit {
            action: RunningTaskExitAction::RunInBackground,
            thread_id,
        } if thread_id == parent_thread_id
    );
    let control = app.handle_event(&mut tui, &mut app_server, event).await?;

    assert_matches!(control, AppRunControl::Exit(ExitReason::UserRequested));
    assert!(app.side_threads.contains_key(&side_thread_id));
    assert!(op_rx.try_recv().is_err());
    assert!(app_event_rx.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn exit_interrupts_before_requesting_shutdown() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    // The UI fixture's placeholder cwd does not exist on clean runners.
    // This test starts a real shell, so keep its working directory alive.
    let cwd = tempdir()?;
    app.config.cwd = cwd.path().abs();
    prepare_running_local_daemon(&mut app)?;
    app.chat_widget
        .set_feature_enabled(Feature::Goals, /*enabled*/ true);
    let (mut app_server, mut tui) =
        prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;
    let started = app_server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.active_thread_id = Some(thread_id);
    app.chat_widget
        .handle_thread_session(started.session.clone());
    let goal = crate::test_support::thread_goal_set(
        &app_server,
        thread_id,
        Some("test goal".to_string()),
        Some(codex_app_server_protocol::ThreadGoalStatus::Paused),
        /*token_budget*/ None,
    )
    .await?
    .goal;
    app.chat_widget.handle_server_notification(
        ServerNotification::ThreadGoalUpdated(
            codex_app_server_protocol::ThreadGoalUpdatedNotification {
                thread_id: thread_id.to_string(),
                turn_id: None,
                goal: codex_app_server_protocol::ThreadGoal {
                    status: codex_app_server_protocol::ThreadGoalStatus::Active,
                    ..goal
                },
            },
        ),
        /*replay_kind*/ None,
    );
    // Keep the command alive until interruption, even on a busy runner. Dropping
    // the directory also releases it if the test fails before reaching the exit.
    let running = tempdir()?;
    let running_path = running.path().to_string_lossy();
    let command = if cfg!(windows) {
        format!(
            "Write-Output 'exit-test-ready'; while (Test-Path -LiteralPath '{}') {{ Start-Sleep -Milliseconds 100 }}",
            running_path.replace('\'', "''"),
        )
    } else {
        format!(
            "printf 'exit-test-ready\\n'; while [ -d {} ]; do sleep 0.1; done",
            shlex::try_quote(&running_path)?,
        )
    };
    app.submit_thread_op(
        &mut app_server,
        thread_id,
        AppCommand::RunUserShellCommand { command },
    )
    .await?;
    let turn_id = time::timeout(Duration::from_secs(/*secs*/ 10), async {
        let mut turn_id = None;
        let mut output = String::new();
        loop {
            let event = app_server
                .next_event()
                .await
                .expect("app-server event stream should remain open");
            if let codex_app_server_client::AppServerEvent::ServerNotification(notification) =
                event.event
            {
                match notification.as_ref() {
                    ServerNotification::TurnStarted(notification)
                        if notification.thread_id == thread_id.to_string() =>
                    {
                        turn_id = Some(notification.turn.id.clone());
                    }
                    ServerNotification::CommandExecutionOutputDelta(notification)
                        if notification.thread_id == thread_id.to_string() =>
                    {
                        output.push_str(&notification.delta);
                    }
                    ServerNotification::TurnCompleted(notification)
                        if notification.thread_id == thread_id.to_string() =>
                    {
                        panic!("shell command ended before interruption: {notification:?}");
                    }
                    _ => {}
                }
            }
            if output.contains("exit-test-ready")
                && let Some(turn_id) = turn_id.as_ref()
            {
                break turn_id.clone();
            }
        }
    })
    .await
    .expect("shell command should be running before interruption");
    app.thread_event_channels.insert(
        thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            started.session,
            vec![test_turn(&turn_id, TurnStatus::InProgress, Vec::new())],
        ),
    );
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, &turn_id),
        /*replay_kind*/ None,
    );
    while app_event_rx.try_recv().is_ok() {}
    while op_rx.try_recv().is_ok() {}
    let control = app
        .handle_event(
            &mut tui,
            &mut app_server,
            AppEvent::RunningTaskExit {
                action: RunningTaskExitAction::Exit,
                thread_id,
            },
        )
        .await?;

    assert_matches!(control, AppRunControl::Continue);
    assert!(op_rx.try_recv().is_err());
    app.settle_turn_submissions(&mut app_server).await?;
    let exit_event = app_event_rx.try_recv()?;
    assert_matches!(exit_event, AppEvent::Exit(ExitMode::ShutdownAfterInterrupt));
    let control = app
        .handle_event(&mut tui, &mut app_server, exit_event)
        .await?;
    assert_matches!(control, AppRunControl::Continue);
    app.settle_turn_submissions(&mut app_server).await?;
    let ready = app_event_rx.try_recv()?;
    let AppRunControl::Exit(reason) = app.handle_event(&mut tui, &mut app_server, ready).await?
    else {
        panic!("stop-and-exit must exit");
    };
    assert_matches!(reason, ExitReason::TurnInterrupted);
    let output = app
        .exit_info(reason)
        .format_exit_messages(/*color_enabled*/ false)
        .join("\n")
        .replace(&thread_id.to_string(), "THREAD_ID");
    assert_snapshot!("interrupted_disconnect_exit", output);
    Ok(())
}

#[tokio::test]
async fn daemon_ctrl_c_closes_running_side_thread_and_returns_to_parent() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    prepare_local_daemon_thread(&mut app)?;
    let (mut app_server, mut tui) =
        prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;
    let started = app_server.start_thread(&app.config).await?;
    let parent_thread_id = started.session.thread_id;
    let side = app_server.start_thread(&app.config).await?;
    let side_thread_id = side.session.thread_id;
    app.active_thread_id = Some(side_thread_id);
    app.chat_widget.handle_thread_session(side.session);
    app.chat_widget
        .handle_server_notification(turn_started_notification(side_thread_id, "turn-1"), None);
    app.primary_thread_id = Some(parent_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));
    app.thread_event_channels.insert(
        parent_thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            started.session,
            started.turns,
        ),
    );

    open_running_task_exit_menu(&mut app, &mut tui, &mut app_server).await;
    app.settle_navigation(&mut tui, &mut app_server).await?;
    app.settle_turn_submissions(&mut app_server).await?;

    assert!(app.chat_widget.no_modal_or_popup_active());
    assert_eq!(app.active_thread_id, Some(parent_thread_id));
    assert!(!app.side_threads.contains_key(&side_thread_id));
    assert!(op_rx.try_recv().is_err());
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn daemon_ctrl_c_hides_background_exit_for_running_background_side_thread() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let parent_thread_id = prepare_local_daemon_thread(&mut app)?;
    let side_thread_id = ThreadId::new();
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));
    app.thread_event_channels.insert(
        side_thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            test_thread_session(side_thread_id, test_path_buf("/tmp/project")),
            vec![test_turn("side-turn", TurnStatus::InProgress, Vec::new())],
        ),
    );
    let (mut app_server, mut tui) =
        prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;

    open_running_task_exit_menu(&mut app, &mut tui, &mut app_server).await;
    assert_snapshot!(render_bottom_popup(&app.chat_widget, /*width*/ 90), @r"
      Task is still running
      Choose what happens to the current task.


    › 1. Cancel task  Stop the current task and stay in Codex
      2. Exit         Stop the current task and exit Codex

      enter select · esc back
    ");
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let event = app_event_rx.try_recv()?;

    assert_matches!(
        event,
        AppEvent::RunningTaskExit {
            action: RunningTaskExitAction::Exit,
            thread_id,
        } if thread_id == side_thread_id
    );
    assert!(op_rx.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn daemon_ctrl_c_hides_background_exit_with_queued_follow_up() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    prepare_running_local_daemon(&mut app)?;
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(
        app.chat_widget.queued_user_message_texts(),
        vec!["queued follow-up".to_string()]
    );
    let (mut app_server, mut tui) =
        prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;

    open_running_task_exit_menu(&mut app, &mut tui, &mut app_server).await;

    assert!(!render_bottom_popup(&app.chat_widget, /*width*/ 90).contains("Run in background"));
    assert!(op_rx.try_recv().is_err());
    assert!(app_event_rx.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn non_daemon_ctrl_c_keeps_interrupt_behavior() -> Result<()> {
    for target in [
        AppServerTarget::Embedded,
        AppServerTarget::Remote {
            endpoint: crate::RemoteAppServerEndpoint::WebSocket {
                websocket_url: "ws://127.0.0.1:4500".to_string(),
                auth_token: None,
            },
        },
    ] {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        prepare_running_local_daemon(&mut app)?;
        app.app_server_target = target;
        let (mut app_server, mut tui) =
            prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;

        open_running_task_exit_menu(&mut app, &mut tui, &mut app_server).await;

        assert!(app.chat_widget.no_modal_or_popup_active());
        assert_matches!(op_rx.try_recv(), Ok(Op::Interrupt));
        assert!(app_event_rx.try_recv().is_err());
    }
    Ok(())
}

#[tokio::test]
async fn daemon_ctrl_c_clears_draft_without_interrupting() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    prepare_running_local_daemon(&mut app)?;
    app.chat_widget.apply_external_edit("draft".to_string());
    let (mut app_server, mut tui) =
        prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;

    open_running_task_exit_menu(&mut app, &mut tui, &mut app_server).await;

    assert!(app.chat_widget.no_modal_or_popup_active());
    assert!(app.chat_widget.composer_is_empty());
    assert!(app.chat_widget.is_task_running_for_test());
    assert!(op_rx.try_recv().is_err());
    while let Ok(event) = app_event_rx.try_recv() {
        assert_matches!(event, AppEvent::AppendMessageHistoryEntry { .. });
    }
    Ok(())
}

#[tokio::test]
async fn daemon_ctrl_c_clears_paste_burst_without_interrupting() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    prepare_running_local_daemon(&mut app)?;
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(app.chat_widget.composer_text_with_pending().is_empty());
    assert!(!app.chat_widget.composer_is_empty());
    let (mut app_server, mut tui) =
        prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;

    open_running_task_exit_menu(&mut app, &mut tui, &mut app_server).await;

    assert!(app.chat_widget.no_modal_or_popup_active());
    assert!(app.chat_widget.composer_is_empty());
    assert!(app.chat_widget.is_task_running_for_test());
    assert!(op_rx.try_recv().is_err());
    while let Ok(event) = app_event_rx.try_recv() {
        assert_matches!(event, AppEvent::AppendMessageHistoryEntry { .. });
    }
    Ok(())
}

#[tokio::test]
async fn daemon_ctrl_c_during_mcp_startup_does_not_show_background_exit_menu() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let thread_id = prepare_local_daemon_thread(&mut app)?;
    app.chat_widget.handle_server_notification(
        ServerNotification::McpServerStatusUpdated(McpServerStatusUpdatedNotification {
            thread_id: Some(thread_id.to_string()),
            name: "slow".to_string(),
            status: McpServerStartupState::Starting,
            error: None,
            failure_reason: None,
        }),
        /*replay_kind*/ None,
    );
    assert!(app.chat_widget.is_task_running_for_test());
    let (mut app_server, mut tui) =
        prepare_background_exit_test(&app, &mut app_event_rx, &mut op_rx).await?;

    open_running_task_exit_menu(&mut app, &mut tui, &mut app_server).await;

    assert!(app.chat_widget.no_modal_or_popup_active());
    assert_matches!(op_rx.try_recv(), Ok(Op::Interrupt));
    assert!(app_event_rx.try_recv().is_err());
    Ok(())
}
