use super::*;
use crate::app::tests::make_test_app_with_channels;
use crate::app::tests::test_thread_session;
use crossterm::event::KeyCode;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn farm_draft_edits_survive_rejection_view_change_and_discard() {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
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
        uncertain: true,
    };
    app.farm.pending = Some(retained.clone());
    app.chat_widget.insert_str(" with newer edits");
    let edited = app.chat_widget.composer_text_with_pending();
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
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
    assert_eq!(
        app.farm.saved.as_ref().map(|saved| &saved.submission),
        Some(&submission)
    );
    app.restore_farm_draft(&id);
    assert_eq!(app.chat_widget.composer_text_with_pending(), edited);
    app.farm.pending = Some(retained.clone());
    app.discard_farm(&mut tui, &id).await;
    assert_eq!(app.chat_widget.composer_text_with_pending(), edited);
    assert!(app.farm.pending.is_none());

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
    app.farm.pending = Some(retained.clone());
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
    app.farm.pending = Some(retained.clone());
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
    app.farm.pending = Some(retained);
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
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
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
    app.farm.pending = Some(RetainedFarm {
        thread_id: root,
        request_id: id.clone(),
        mode: ThreadFarmMode::FullHistory,
        submission: submission.clone(),
        uncertain: false,
    });
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    app.set_farm_receipt(&mut tui, root, &id, "Farming task…".into());
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
        app.farm
            .pending
            .as_ref()
            .map(|pending| (&pending.request_id, pending.uncertain)),
        Some((&id, true))
    );

    #[cfg(target_os = "linux")]
    {
        // Refuse replacement before checkpoint capture: no UUID is lost or recreated.
        while events.try_recv().is_ok() {}
        assert!(!app.ready_for_idle_restart());
        app.reload_ui().await;
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
            app.farm.pending.as_ref().map(|pending| &pending.request_id),
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
    let root_transcript = std::mem::take(&mut app.transcript_cells);
    let child = ThreadId::new();
    app.active_thread_id = Some(child);
    app.native_history = Default::default();
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
    assert!(app.farm.pending.is_none());
    assert_eq!(app.active_thread_id, Some(child));
    assert!(
        app.transcript_cells
            .iter()
            .zip(&child_transcript)
            .all(|(current, previous)| Arc::ptr_eq(current, previous))
    );
    assert_eq!(app.transcript_cells.len(), child_transcript.len());
    app.restore_farm_receipts(&mut tui, root);
    assert_eq!(app.transcript_cells.len(), child_transcript.len());

    // Existing replay wiring restores the latest root receipt, not the stale pending row.
    app.active_thread_id = Some(root);
    app.transcript_cells = root_transcript;
    app.native_history = Default::default();
    let snapshot = app.thread_event_channels[&root]
        .store
        .lock()
        .await
        .snapshot();
    app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);
    loop {
        if let AppEvent::RestoreFarmReceipts { thread_id } =
            events.try_recv().expect("receipt replay event")
        {
            assert_eq!(thread_id, root);
            app.restore_farm_receipts(&mut tui, thread_id);
            break;
        }
    }
    app.restore_farm_receipts(&mut tui, root);
    assert_eq!(app.active_thread_id, Some(root));
    assert_eq!(app.chat_widget.composer_text_with_pending(), "");
    assert_eq!(app.transcript_cells.len(), before);
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
    insta::assert_snapshot!(text, @"• Farmed to /root/farm_1 · child-id");
    while let Ok(operation) = ops.try_recv() {
        assert!(!matches!(
            operation,
            AppCommand::UserTurn { .. } | AppCommand::Interrupt
        ));
    }
}
