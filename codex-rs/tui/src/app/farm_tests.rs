use super::*;
use crate::AppServerTarget;
use crate::app::SideParentStatus;
use crate::app::SideThreadState;
use crate::app::tests::make_test_app_with_channels;
use crate::app::tests::test_thread_session;
use crate::app_command::AppCommand;
use crate::history_cell;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnStatus;
use crossterm::event::KeyCode;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn farm_draft_edits_survive_rejection_view_change_and_discard() {
    let (mut app, mut events, _ops) = Box::pin(make_test_app_with_channels()).await;
    let root = ThreadId::new();
    app.primary_thread_id = Some(root);
    app.active_thread_id = Some(root);
    app.chat_widget
        .handle_thread_session_quiet(test_thread_session(root, app.config.cwd.to_path_buf()));
    app.chat_widget.insert_str("/farm original task");
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    let submission = loop {
        if let AppEvent::Farm { submission, .. } = events.try_recv().expect("farm event") {
            break submission;
        }
    };
    let original_input = app
        .chat_widget
        .capture_thread_input_state()
        .expect("original input");
    let id = Uuid::new_v4().to_string();
    let retained = RetainedFarm {
        thread_id: root,
        request_id: id.clone(),
        mode: ThreadFarmMode::FullHistory,
        submission: submission.clone(),
    };
    app.farm.state = FarmRequestState::Uncertain(retained.clone());
    app.chat_widget.insert_str(" with newer edits");
    let edited = app.chat_widget.composer_text_with_pending();
    let mut tui = tui::test_support::make_test_tui().expect("test tui");
    app.open_farm_recovery(root);
    assert_eq!(app.chat_widget.composer_text_with_pending(), edited);
    app.chat_widget.handle_key_event(KeyCode::Esc.into());
    app.finish_farm(
        &mut tui,
        id.clone(),
        Err(FarmFailure::Rejected("validation".into())),
    )
    .await;
    assert_eq!(app.chat_widget.composer_text_with_pending(), edited);
    assert!(matches!(app.farm.state, FarmRequestState::Idle));
    app.farm.state = FarmRequestState::Uncertain(retained.clone());
    app.discard_farm(&mut tui, &id).await;
    assert_eq!(app.chat_widget.composer_text_with_pending(), edited);
    assert!(matches!(app.farm.state, FarmRequestState::Idle));

    // Hidden origins follow the same rule: only an exact matching submitted draft is cleared.
    let hidden_edited = app
        .chat_widget
        .capture_thread_input_state()
        .expect("edited input");
    app.ensure_thread_channel(root)
        .store
        .lock()
        .await
        .input_state = Some(hidden_edited.clone());
    let other = ThreadId::new();
    app.primary_thread_id = Some(other);
    app.active_thread_id = Some(other);
    app.farm.state = FarmRequestState::Uncertain(retained.clone());
    app.finish_farm(
        &mut tui,
        id.clone(),
        Err(FarmFailure::Rejected("validation".into())),
    )
    .await;
    assert_eq!(
        app.thread_event_channels[&root]
            .store
            .lock()
            .await
            .input_state,
        Some(hidden_edited.clone())
    );
    app.farm.state = FarmRequestState::Uncertain(retained.clone());
    app.discard_farm(&mut tui, &id).await;
    assert_eq!(
        app.thread_event_channels[&root]
            .store
            .lock()
            .await
            .input_state,
        Some(hidden_edited)
    );
    app.ensure_thread_channel(root)
        .store
        .lock()
        .await
        .input_state = Some(original_input.clone());
    app.farm.state = FarmRequestState::Uncertain(retained);
    app.discard_farm(&mut tui, &id).await;
    let mut cleared = original_input;
    cleared.clear_farm_draft(&submission.draft);
    assert_eq!(
        app.thread_event_channels[&root]
            .store
            .lock()
            .await
            .input_state,
        Some(cleared)
    );
    assert_eq!(app.chat_widget.composer_text_with_pending(), edited);
}

#[tokio::test]
async fn farm_receipt_reconciles_identity_and_draft_without_steering_or_navigation() {
    let (mut app, mut events, mut ops) = Box::pin(make_test_app_with_channels()).await;
    let root = ThreadId::new();
    app.primary_thread_id = Some(root);
    app.active_thread_id = Some(root);
    app.chat_widget
        .handle_thread_session_quiet(test_thread_session(root, app.config.cwd.to_path_buf()));
    app.chat_widget.insert_str("/farm investigate the race");
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    let submission = loop {
        if let AppEvent::Farm {
            thread_id,
            mode,
            submission,
        } = events.try_recv().expect("farm event")
        {
            assert_eq!((thread_id, mode), (root, ThreadFarmMode::FullHistory));
            break submission;
        }
    };
    let draft = app.chat_widget.composer_text_with_pending();
    let id = Uuid::new_v4().to_string();
    app.farm.state = FarmRequestState::Waiting(RetainedFarm {
        thread_id: root,
        request_id: id.clone(),
        mode: ThreadFarmMode::FullHistory,
        submission: submission.clone(),
    });
    let mut tui = tui::test_support::make_test_tui().expect("test tui");
    tui.set_owned_screen(/*owned*/ true).unwrap();
    let message = |text: &str| {
        ServerNotification::ItemCompleted(codex_app_server_protocol::ItemCompletedNotification {
            thread_id: root.to_string(),
            turn_id: "farm-turn".into(),
            completed_at_ms: 0,
            item: ThreadItem::UserMessage {
                source_response_item_id: None,
                id: text.into(),
                client_id: None,
                content: vec![codex_app_server_protocol::UserInput::Text {
                    text: text.into(),
                    text_elements: Vec::new(),
                }],
            },
        })
    };
    app.ensure_thread_channel(root)
        .store
        .lock()
        .await
        .push_notification(message("Before farming"));
    app.set_farm_receipt(&mut tui, root, &id, "Farming task…".into())
        .await;
    app.ensure_thread_channel(root)
        .store
        .lock()
        .await
        .push_notification(message("After farming"));
    let before = app.transcript_cells.len();
    app.finish_farm(
        &mut tui,
        Uuid::new_v4().to_string(),
        Err(FarmFailure::Rejected("stale".into())),
    )
    .await;
    assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
    app.finish_farm(
        &mut tui,
        id.clone(),
        Err(FarmFailure::Uncertain("transport disconnected".into())),
    )
    .await;
    assert_eq!(app.transcript_cells.len(), before);
    assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
    assert_eq!(
        match &app.farm.state {
            FarmRequestState::Uncertain(pending) => Some(&pending.request_id),
            FarmRequestState::Idle | FarmRequestState::Waiting(_) => None,
        },
        Some(&id)
    );

    #[cfg(target_os = "linux")]
    {
        // Refuse replacement before checkpoint capture: no UUID is lost or recreated.
        while events.try_recv().is_ok() {}
        assert!(!app.ready_for_idle_restart());
        let _ = app.reload_ui(/*idle_candidate*/ None).await;
        let error = loop {
            if let AppEvent::InsertHistoryCell(cell) = events.try_recv().expect("reload error") {
                break cell
                    .raw_lines()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        };
        insta::assert_snapshot!(error, @"■ Resolve/discard farm request before reload.");
        assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
        assert_eq!(
            match &app.farm.state {
                FarmRequestState::Waiting(pending) | FarmRequestState::Uncertain(pending) =>
                    Some(&pending.request_id),
                FarmRequestState::Idle => None,
            },
            Some(&id)
        );
    }

    let original_input = app.chat_widget.capture_thread_input_state();
    let session = test_thread_session(root, app.config.cwd.to_path_buf());
    {
        let channel = app.ensure_thread_channel(root);
        let mut store = channel.store.lock().await;
        store.session = Some(session);
        store.input_state = original_input;
    }
    app.transcript_cells.clear();
    let child = ThreadId::new();
    app.active_thread_id = Some(child);
    app.native_history = super::super::native_history::NativeHistory::default();
    app.insert_history_cell(
        &mut tui,
        Box::new(history_cell::new_info_event(
            "Child conversation".into(),
            /*hint*/ None,
        )),
    );
    let child_transcript = app.transcript_cells.clone();
    app.finish_farm(
        &mut tui,
        id.clone(),
        Ok(ThreadFarmResponse {
            child_thread_id: "child-id".into(),
            agent_path: "/root/farm_1".into(),
        }),
    )
    .await;
    assert!(matches!(app.farm.state, FarmRequestState::Idle));
    assert_eq!(app.active_thread_id, Some(child));
    assert!(
        app.transcript_cells
            .iter()
            .zip(&child_transcript)
            .all(|(current, previous)| Arc::ptr_eq(current, previous))
    );
    assert_eq!(app.transcript_cells.len(), child_transcript.len());
    app.restore_farm_receipt(&mut tui, root, &id);
    assert_eq!(app.transcript_cells.len(), child_transcript.len());

    // Rebuild from actual buffered events, not saved rendered cells. Repeated switches
    // must update the original receipt position instead of appending it after newer text.
    app.active_thread_id = Some(root);
    app.native_history = super::super::native_history::NativeHistory::default();
    let snapshot = app.thread_event_channels[&root]
        .store
        .lock()
        .await
        .snapshot();
    for _ in 0..2 {
        app.transcript_cells.clear();
        app.replay_thread_snapshot(snapshot.clone(), /*resume_restored_queue*/ false);
        while let Ok(event) = events.try_recv() {
            match event {
                AppEvent::InsertHistoryCell(cell) => app.insert_history_cell(&mut tui, cell),
                AppEvent::RestoreFarmReceipt {
                    thread_id,
                    request_id,
                } => {
                    assert_eq!((thread_id, &request_id), (root, &id));
                    app.restore_farm_receipt(&mut tui, thread_id, &request_id);
                }
                _ => {}
            }
        }
        app.restore_farm_receipt(&mut tui, root, &id);
        let relevant: Vec<_> = app
            .transcript_cells
            .iter()
            .flat_map(|cell| cell.display_lines(/*width*/ 80))
            .map(|line| line.to_string())
            .filter(|line| line.contains("farming") || line.contains("Farmed to"))
            .map(|line| line.trim().to_owned())
            .collect();
        insta::assert_snapshot!("farm_receipt_chronology", relevant.join("\n"));
        assert_eq!(
            app.transcript_cells
                .iter()
                .filter(|cell| cell.as_any().is::<FarmReceipt>())
                .count(),
            1
        );
    }
    assert_eq!(app.active_thread_id, Some(root));
    assert_eq!(app.chat_widget.composer_text_with_pending(), "");
    let receipt = app
        .transcript_cells
        .iter()
        .find(|cell| cell.as_any().is::<FarmReceipt>())
        .expect("one receipt");
    let text = receipt
        .display_lines(/*width*/ 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(text, @"Farmed to /root/farm_1 · child-id");

    #[cfg(target_os = "linux")]
    {
        app.app_server_target = AppServerTarget::LocalDaemon {
            endpoint: RemoteAppServerEndpoint::UnixSocket {
                socket_path: app.config.codex_home.join("test-server.sock"),
            },
            allow_embedded_fallback: false,
        };
        let mut turn = Turn {
            id: "completed-turn".into(),
            root_turn_id: None,
            items_view: codex_app_server_protocol::TurnItemsView::Full,
            items: Vec::new(),
            status: TurnStatus::InProgress,
            error: None,
            started_at: Some(0),
            completed_at: None,
            duration_ms: None,
        };
        app.chat_widget.handle_server_notification(
            ServerNotification::TurnStarted(codex_app_server_protocol::TurnStartedNotification {
                thread_id: root.to_string(),
                turn: turn.clone(),
            }),
            /*replay_kind*/ None,
        );
        turn.status = TurnStatus::Completed;
        turn.completed_at = Some(1);
        app.chat_widget.handle_server_notification(
            ServerNotification::TurnCompleted(
                codex_app_server_protocol::TurnCompletedNotification {
                    thread_id: root.to_string(),
                    turn,
                },
            ),
            /*replay_kind*/ None,
        );
        assert!(app.ready_for_idle_restart());

        // Toggling to the parent leaves the ephemeral side open, even without a draft or status.
        // Neither replacement path may mistake that quiet state for disposable UI state.
        let side = ThreadId::new();
        app.side_threads.insert(side, SideThreadState::new(root));
        let mut errors = Vec::new();
        for displayed in [root, side] {
            app.active_thread_id = Some(displayed);
            assert!(!app.ready_for_idle_restart());
            while events.try_recv().is_ok() {}
            let error = app.reload_ui(/*idle_candidate*/ None).await.unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
            let message = loop {
                if let AppEvent::InsertHistoryCell(cell) = events.try_recv().expect("reload error")
                {
                    break cell
                        .raw_lines()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            };
            errors.push(message);
            assert_eq!(app.side_threads[&side].parent_thread_id, root);
            assert_eq!(app.side_threads[&side].parent_status, None);
            assert_eq!(app.active_thread_id, Some(displayed));
        }
        insta::assert_snapshot!(errors.join("\n"), @r"
        ■ Close the side conversation before reloading (ctrl+c in its empty composer).
        ■ Close the side conversation before reloading (ctrl+c in its empty composer).
        ");
        app.chat_widget.insert_str("unsent side question");
        app.side_threads.get_mut(&side).unwrap().parent_status = Some(SideParentStatus::NeedsInput);
        let input = app.chat_widget.capture_thread_input_state();
        assert!(!app.ready_for_idle_restart());
        assert_eq!(
            app.reload_ui(/*idle_candidate*/ None)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(app.chat_widget.capture_thread_input_state(), input);
        assert_eq!(
            app.side_threads[&side].parent_status,
            Some(SideParentStatus::NeedsInput)
        );
    }
    while let Ok(operation) = ops.try_recv() {
        assert!(!matches!(
            operation,
            AppCommand::UserTurn { .. } | AppCommand::Interrupt
        ));
    }
}
