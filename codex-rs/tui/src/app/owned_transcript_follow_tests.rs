//! The return control preserves composer geometry and draft across mouse navigation and resize,
//! and live work stays pinned under the transcript's closing rule.

use super::*;
use crate::history_cell::PlainHistoryCell;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn follow_control_click_preserves_draft_caret_and_composer_geometry() -> Result<()> {
    let mut app = crate::app::test_support::make_test_app().await;
    app.local_settings.tui.animations = false;
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.set_owned_screen(/*owned*/ true)?;
    app.chat_widget
        .apply_external_edit("draft stays here".to_string());
    app.transcript_cells = vec![Arc::new(PlainHistoryCell::new(
        (0..40)
            .map(|row| format!("transcript row {row}").into())
            .collect(),
    ))];
    for (width, running) in [(80, false), (40, false), (80, true)] {
        if running {
            // A running block draws in place and again under the rule, named for itself.
            let owner = codex_app_server_protocol::CodeModeOwner {
                thread_id: ThreadId::from_u128(/*value*/ 7),
                exec_call_id: "exec-1".into(),
                cell_id: None,
                started_at_ms: 0,
            };
            let mut reducers = crate::code_mode_transcript::Reducers::default();
            let live = crate::app::thread_events::ThreadEventChannel::new(32);
            let mut reduce = |item| {
                let notification = codex_app_server_protocol::ServerNotification::ItemCompleted(
                    codex_app_server_protocol::ItemCompletedNotification::new(
                        owner.thread_id.to_string(),
                        String::new(),
                        codex_app_server_protocol::ThreadItemPayload::CodeModeOwned(
                            codex_app_server_protocol::CodeModeOwned::new(
                                owner.clone(),
                                Box::new(item),
                            ),
                        ),
                        None,
                        0,
                    ),
                );
                live.store
                    .try_lock()
                    .unwrap()
                    .push_notification(notification.clone());
                reducers
                    .admit(
                        owner.thread_id,
                        crate::code_mode_transcript::Admission::notification(
                            &notification,
                            crate::code_mode_transcript::Source::Live,
                        )
                        .unwrap()
                        .unwrap(),
                        &app.config.cwd,
                        /*animations*/ false,
                        &|_| crate::multi_agents::AgentMetadata::default(),
                        crate::history_cell::ImagePathOrigin::Local,
                    )
                    .unwrap()
                    .block
            };
            reduce(codex_app_server_protocol::ThreadItemPayload::CodeMode(
                codex_app_server_protocol::CodeModeItem {
                    id: "code-mode:exec-1:start".into(),
                    cell_id: None,
                    exec_call_id: "exec-1".into(),
                    created_at: 0,
                    event: codex_app_server_protocol::CodeModeEvent::Started {
                        call_id: "exec-1".into(),
                        script: None,
                    },
                },
            ));
            let block = reduce(
                codex_app_server_protocol::ThreadItemPayload::CommandExecution {
                    sandbox_type: None,
                    model_context: None,
                    id: "call-test".into(),
                    plugin_id: None,
                    script_path: None,
                    command: "cargo test".into(),
                    cwd: app.config.cwd.clone().into(),
                    process_id: None,
                    source: codex_app_server_protocol::CommandExecutionSource::Agent,
                    status: codex_app_server_protocol::CommandExecutionStatus::InProgress,
                    command_actions: Vec::new(),
                    aggregated_output: None,
                    exit_code: None,
                    duration_ms: None,
                },
            );
            app.transcript_cells.push(block);
            app.active_thread_id = Some(owner.thread_id);
            app.thread_event_channels.insert(owner.thread_id, live);
            app.chat_widget.handle_server_notification(
                codex_app_server_protocol::ServerNotification::TurnStarted(
                    codex_app_server_protocol::TurnStartedNotification {
                        thread_id: ThreadId::new().to_string(),
                        turn: codex_app_server_protocol::Turn {
                            id: "turn".into(),
                            root_turn_id: None,
                            items_view: codex_app_server_protocol::TurnItemsView::Full,
                            items: Vec::new(),
                            status: codex_app_server_protocol::TurnStatus::InProgress,
                            error: None,
                            started_at: None,
                            completed_at: None,
                            duration_ms: None,
                        },
                    },
                ),
                /*replay_kind*/ None,
            );
        }
        // Leave history visible for the return-control interaction; short resizes below may hide it.
        let size = Size::new(width, if running { 14 } else { 15 });
        tui.screen_size_for_event(&TuiEvent::Resize(size))?;
        app.transcript_view.jump_to_latest();
        let bottom = app.render_owned_transcript(&mut tui, size)?;
        let cursor = tui.terminal.last_known_cursor_pos;
        if running {
            let buffer = crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal);
            insta::assert_snapshot!(
                "running_without_composer_hint",
                crate::chatwidget::tests::helpers::normalize_snapshot_paths(
                    super::tests::buffer_text(buffer)
                ),
            );
        }
        app.transcript_view
            .scroll(&app.transcript_cells, /*rows*/ -10);
        tui.screen_size_for_event(&TuiEvent::Resize(size))?;
        app.render_owned_transcript(&mut tui, size)?;
        let scrolled_cursor = tui.terminal.last_known_cursor_pos;
        assert_eq!(scrolled_cursor, cursor);
        let buffer = crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal);
        let scrolled = super::tests::buffer_text(buffer);
        let first_row = scrolled
            .lines()
            .find(|row| row.contains("transcript row"))
            .expect("visible transcript row");
        // Keep history visible above the native ruled composer in the inset, too.
        let inset_size = Size::new(width.max(38), 24);
        tui.screen_size_for_event(&TuiEvent::Resize(inset_size))?;
        let area = Rect::new(
            /*x*/ 4, /*y*/ 1, /*width*/ 34, /*height*/ 22,
        );
        let frames = tui.frame_requester();
        let mut painted = Default::default();
        tui.draw(inset_size.height, |frame| {
            let prepared = app.prepare_conversation(frame.area(), Some(area), &frames);
            painted = app.paint_conversation(frame, &prepared);
        })?;
        app.finish_conversation(&mut tui, painted)?;
        let buffer = crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal);
        let resized = super::tests::buffer_text(buffer);
        assert_eq!(
            resized
                .lines()
                .find(|row| row.contains("transcript row"))
                .map(str::trim),
            Some(first_row.trim()),
            "offset conversation: {resized}"
        );
        assert!(resized.contains("draft stays here"));
        tui.screen_size_for_event(&TuiEvent::Resize(size))?;
        let mut painted = Default::default();
        tui.screen_size_for_event(&TuiEvent::Resize(size))?;
        tui.draw(size.height, |frame| {
            let prepared =
                app.prepare_conversation(frame.area(), /*conversation_inset*/ None, &frames);
            painted = app.paint_conversation(frame, &prepared);
        })?;
        app.finish_conversation(&mut tui, painted)?;
        for height in [24, 9, 8, size.height] {
            let resized = Size::new(width, height);
            tui.screen_size_for_event(&TuiEvent::Resize(resized))?;
            app.render_owned_transcript(&mut tui, resized)?;
            let resized_cursor = tui.terminal.last_known_cursor_pos;
            assert_eq!(
                (
                    app.chat_widget.composer_text_with_pending(),
                    resized_cursor.x,
                    resized.height - resized_cursor.y,
                    app.transcript_view.is_following(),
                ),
                (
                    "draft stays here".to_string(),
                    cursor.x,
                    size.height - scrolled_cursor.y,
                    false
                )
            );
            if running && height == 9 {
                let buffer =
                    crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal);
                insta::assert_snapshot!(
                    "running_follow_control_short_terminal",
                    crate::chatwidget::tests::helpers::normalize_snapshot_paths(
                        super::tests::buffer_text(buffer)
                    ),
                );
            }
        }
        let buffer = crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal);
        if running {
            insta::assert_snapshot!(
                "running_follow_control",
                crate::chatwidget::tests::helpers::normalize_snapshot_paths(
                    super::tests::buffer_text(buffer)
                ),
            );
        } else {
            insta::assert_snapshot!(
                format!("idle_follow_control_{width}"),
                crate::chatwidget::tests::helpers::normalize_snapshot_paths(
                    super::tests::buffer_text(buffer)
                ),
            );
        }
        let (column, row) = buffer
            .content()
            .iter()
            .position(|cell| cell.symbol() == "↓")
            .map(|index| buffer.pos_of(index))
            .expect("return control");
        for interrupted in [true, false] {
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                if interrupted && kind == MouseEventKind::Up(MouseButton::Left) {
                    app.handle_owned_transcript_event(&mut tui, &mut server, &TuiEvent::FocusLost)?;
                    tui.screen_size_for_event(&TuiEvent::Resize(size))?;
                    app.render_owned_transcript(&mut tui, size)?;
                }
                tui.screen_size_for_event(&TuiEvent::Resize(size))?;
                let handled = app.handle_owned_transcript_event(
                    &mut tui,
                    &mut server,
                    &TuiEvent::Mouse(MouseEvent {
                        kind,
                        column,
                        row,
                        modifiers: KeyModifiers::NONE,
                    }),
                )?;
                assert!(handled || interrupted);
            }
            assert_eq!(app.transcript_view.is_following(), !interrupted);
        }
        tui.screen_size_for_event(&TuiEvent::Resize(size))?;
        assert_eq!(app.render_owned_transcript(&mut tui, size)?, bottom);
        assert_eq!(
            (
                app.chat_widget.composer_text_with_pending(),
                tui.terminal.last_known_cursor_pos
            ),
            ("draft stays here".to_string(), cursor)
        );
        assert!(app.chat_widget.queued_user_message_texts().is_empty());
        if running {
            app.transcript_view.show_copy_feedback(
                &Ok(crate::clipboard_copy::CopyStatus::Confirmed),
                /*characters*/ 24,
            );
            let mut snapshots = Vec::new();
            for (width, height) in [(80, 12), (18, 14), (6, 18), (80, 6)] {
                let size = Size::new(width, height);
                tui.screen_size_for_event(&TuiEvent::Resize(size))?;
                app.render_owned_transcript(&mut tui, size)?;
                let buffer =
                    crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal);
                snapshots.push(format!(
                    "{width} columns, {height} rows\n{}",
                    super::tests::buffer_text(buffer)
                ));
            }

            insta::assert_snapshot!(
                "running_copy_feedback_spacing",
                crate::chatwidget::tests::helpers::normalize_working_snapshot(
                    snapshots.join("\n\n")
                ),
            );
        }
    }
    tui.set_owned_screen(/*owned*/ false)?;
    assert!(!app.handle_owned_transcript_event(
        &mut tui,
        &mut server,
        &TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: 7,
            modifiers: KeyModifiers::NONE,
        })
    )?);
    server.shutdown().await?;
    Ok(())
}
