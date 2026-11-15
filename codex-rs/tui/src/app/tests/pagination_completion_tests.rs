//! Older page completion preserves answer order, navigation intent, and pending request ownership.

use super::session_lifecycle_requests::start_recording_app_server;
use super::*;
use crate::app_server_session::HistoryPage;
use crate::pager_overlay::TranscriptHistoryState;
use app_test_support::create_fake_paginated_rollout;
use app_test_support::rollout_path;
use chrono::TimeZone;

use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::user_input::UserInput as CoreUserInput;
use codex_state::SqliteConfig;
use pretty_assertions::assert_eq;

pub(super) async fn completed_history_app(
    names: &[&str],
) -> Result<(Box<App>, tempfile::TempDir, ThreadId)> {
    let mut app = make_test_app().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.sqlite = SqliteConfig::new_for_testing(codex_home.path().abs());
    // The inline scrollback row cap fixes the page boundary used by this regression.
    app.local_settings.transcript_mode = crate::transcript_mode::TranscriptMode::Terminal;
    app.local_settings.tui.alternate_screen = codex_config::types::AltScreenMode::Never;
    app.local_settings.tui.terminal_resize_reflow_max_rows = Some(2);
    let completed_at = chrono::Local
        .with_ymd_and_hms(
            /*year*/ 2000, /*month*/ 9, /*day*/ 6, /*hour*/ 14,
            /*min*/ 32, /*sec*/ 0,
        )
        .single()
        .expect("unambiguous local completion time");
    let timestamp = completed_at.to_rfc3339();
    let filename_timestamp = "2000-09-06T14-32-00";
    let thread_id = create_fake_paginated_rollout(
        codex_home.path(),
        filename_timestamp,
        &timestamp,
        "completed pagination",
        Some(app.config.model_provider_id.as_str()),
        /*git_info*/ None,
    )
    .map_err(|error| color_eyre::eyre::eyre!(error))?;
    let path = rollout_path(codex_home.path(), filename_timestamp, &thread_id);
    let thread_id = ThreadId::from_string(&thread_id)?;
    let mut records = std::fs::read_to_string(&path)?
        .lines()
        .take(/*n*/ 1)
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    for (index, name) in names.iter().enumerate() {
        let turn_id = format!("turn-{index}");
        let finished = completed_at.timestamp() + index as i64 * 60;
        let mut events = vec![EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: turn_id.clone(),
            root_turn_id: None,
            trace_id: None,
            started_at: Some(finished - 125),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        })];
        let items = [
            TurnItem::UserMessage(UserMessageItem {
                source_response_item_id: None,
                id: format!("prompt-{index}"),
                client_id: None,
                content: vec![CoreUserInput::Text {
                    text: format!("{name} prompt"),
                    text_elements: Vec::new(),
                }],
            }),
            TurnItem::AgentMessage(AgentMessageItem {
                moderation: None,
                id: format!("answer-{index}"),
                content: vec![AgentMessageContent::Text {
                    text: format!("{name} answer"),
                }],
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            }),
        ];
        events.extend(items.into_iter().map(|item| {
            EventMsg::ItemCompleted(ItemCompletedEvent {
                content_token_count: None,
                thread_id,
                turn_id: turn_id.clone(),
                item,
                started_at_ms: None,
                completed_at_ms: finished * 1_000,
            })
        }));
        events.push(EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id,
            last_agent_message: Some(format!("{name} answer")),
            error: None,
            started_at: Some(finished - 125),
            completed_at: Some(finished),
            duration_ms: Some(125_000),
            time_to_first_token_ms: None,
        }));
        for event in events {
            records.push(serde_json::json!({
                "timestamp": timestamp,
                "ordinal": records.len(),
                "type": "event_msg",
                "payload": event,
            }));
        }
    }
    let records = records
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{records}\n"))?;
    Ok((app, codex_home, thread_id))
}

#[tokio::test]
async fn older_page_updates_the_parked_parent_without_moving_focus() -> Result<()> {
    let (mut app, _codex_home, thread_id) = completed_history_app(&["Oldest", "Newest"]).await?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let started = app_server
        .resume_thread(
            &app.local_settings,
            app.config.clone(),
            thread_id,
            crate::app_server_session::ResumeModelSettings::RestoreFromThread,
        )
        .await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    let (event_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    app.app_event_tx = crate::app_event_sender::AppEventSender::new(event_tx);
    let mut tui = crate::tui::test_support::make_test_tui()?;
    enable_side_retention(&mut app)?;
    tui.set_owned_screen(/*owned*/ true)?;

    app.scrollback_has_older_history = app_server.has_older_history(thread_id);
    app.transcript_cells = vec![plain_line_cell("Newest answer")];
    app.transcript_view.history = TranscriptHistoryState::Partial;
    let child = ThreadId::new();
    app.side_threads
        .insert(child, SideThreadState::new(thread_id));
    let display = app.chat_widget.take_side_display(/*width*/ 100);
    boxed_future!(app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::ParkSideCompanion {
            pair: child,
            thread_id,
            display
        },
    ))
    .await?;
    app.chat_widget
        .handle_side_thread_session(test_thread_session(child, app.config.cwd.to_path_buf()));
    app.active_thread_id = Some(child);
    app.sidebar_mut().show(
        sidebar::SidebarContent::Chat(child),
        sidebar::PaneFocus::Sidebar,
    );
    let size = Size::new(/*width*/ 120, /*height*/ 18);
    tui.terminal = crate::tui::Terminal::with_screen_size_and_cursor_position_for_test(
        ratatui::backend::CrosstermBackend::new(crate::custom_terminal::output::stdout()),
        size,
        Default::default(),
    );
    crate::app::test_support::settle_until_painted(
        &mut app,
        &mut tui,
        |app, tui| side_frame(app, tui, size),
        |frame| frame.contains("Newest answer"),
    )
    .await?;
    let wheel = mouse(MouseEventKind::ScrollUp, /*column*/ 4, /*row*/ 3);
    assert!(app.handle_owned_transcript_event(&mut tui, &mut app_server, &wheel)?);
    tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
        loop {
            let event = events.recv().await.expect("older page completion");
            let parent_page = matches!(&event, AppEvent::OlderThreadHistoryLoaded { thread_id: id, .. } if *id == thread_id);
            boxed_future!(app.handle_event(&mut tui, &mut app_server, event)).await?;
            if parent_page { break Ok::<_, color_eyre::Report>(()); }
        }
    }).await??;

    assert_eq!(app.chat_widget.thread_id(), Some(child));
    let pane = &app.side_companions[&thread_id];
    let text = pane
        .cells
        .iter()
        .flat_map(|cell| cell.display_lines(/*width*/ 100))
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Oldest answer"), "{text}");
    assert_eq!(pane.view.history, TranscriptHistoryState::Complete);
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn older_pagination_completion_footers_follow_answers_without_overlap_duplicates()
-> Result<()> {
    let (mut app, _codex_home, thread_id) =
        completed_history_app(&["Oldest", "Middle", "Newest"]).await?;
    let (mut app_server, _requests, proxy) = start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let started = app_server
        .resume_thread(
            &app.local_settings,
            app.config.clone(),
            thread_id,
            crate::app_server_session::ResumeModelSettings::RestoreFromThread,
        )
        .await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    // Snapshot only the prepended pages; the newest turn is already hydrated in the store.
    app.local_settings.tui.terminal_resize_reflow_max_rows = None;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.open_transcript_overlay(&mut tui);
    let mut overlapping_answer = None;
    let mut snapshots = Vec::new();
    for limit in [1, 100] {
        let cursor = app_server
            .begin_older_history_page(thread_id)
            .expect("older page");
        let HistoryPage::Items(mut page) = cursor
            .load(
                &app_server.request_handle(),
                app_server.next_request_id(),
                thread_id,
                limit,
            )
            .await?
        else {
            panic!("item frontier");
        };
        if let Some(answer) = overlapping_answer.take() {
            // The later response repeats the preceding page's completed answer.
            page.data.insert(/*index*/ 0, answer);
        } else {
            overlapping_answer = page.data.first().cloned();
        }
        app.handle_older_history_page(
            &mut tui,
            &mut app_server,
            thread_id,
            &cursor,
            Ok(HistoryPage::Items(page)),
        )
        .await?;
        snapshots.push(
            app.render_transcript_lines_for_reflow(/*width*/ 80)
                .lines
                .iter()
                .map(rendered_line_text)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    // Canonicalize the fixture's afternoon clock while retaining minute differences.
    let afternoon_clock = regex_lite::Regex::new(r"at 14:(\d\d)").unwrap();
    insta::assert_snapshot!(afternoon_clock.replace_all(
        &snapshots.join("\n\n--- next overlapping page ---\n\n"),
        "at 2:$1 PM",
    ));
    app_server.shutdown().await?;
    proxy.await??;

    // Metadata precedes item pages: failures must move with their own items,
    // while a real zero-item failure must never wait for a nonexistent item.
    let (mut app, _codex_home, thread_id) =
        completed_history_app(&["Oldest", "Middle", "Newer", "Newest"]).await?;
    let (chat, tx, mut rx, _ops) = make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat;
    app.app_event_tx = tx;
    let (mut app_server, _requests, proxy) =
        start_recording_app_server(&app.config, None, None).await?;
    let mut started = app_server
        .resume_thread(
            &app.local_settings,
            app.config.clone(),
            thread_id,
            crate::app_server_session::ResumeModelSettings::RestoreFromThread,
        )
        .await?;
    let error = codex_app_server_protocol::TurnError {
        message: "Anthropic tool result has no call ID".to_string(),
        codex_error_info: None,
        additional_details: Some("saved source detail".to_string()),
        misalignment: None,
    };
    for turn in &mut started.turns {
        if turn.id == "turn-0" || turn.id == "turn-2" {
            turn.status = TurnStatus::Failed;
            turn.error = Some(error.clone());
        }
    }
    assert_eq!(started.turns.len(), 4);
    assert!(started.turns[0].items.is_empty());
    let oldest_completed_at = started.turns[0].completed_at;
    let mut empty = test_turn("empty-failure", TurnStatus::Failed, Vec::new());
    empty.items_view = codex_app_server_protocol::TurnItemsView::NotLoaded;
    empty.error = Some(error.clone());
    // Source order, not a guessed clock, owns chronology.
    empty.completed_at = None;
    started.turns.insert(1, empty);
    let session = started.session.clone();
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    // Buffered errors can replay after the completion that already supplied the clock.
    app.chat_widget.handle_server_notification(
        ServerNotification::Error(codex_app_server_protocol::ErrorNotification {
            thread_id: thread_id.to_string(),
            turn_id: "turn-0".to_string(),
            error: error.clone(),
            will_retry: false,
        }),
        Some(ReplayKind::ThreadSnapshot),
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            app.insert_retained_history_cell(&mut tui, Arc::from(cell));
        }
    }
    let failures = |app: &App| {
        app.transcript_cells
            .iter()
            .filter_map(|cell| {
                cell.as_any()
                    .downcast_ref::<history_cell::FailedTurnCell>()
                    .map(|failure| {
                        (
                            failure.turn_id.clone(),
                            failure.completed_at,
                            failure.error.clone(),
                        )
                    })
            })
            .collect::<Vec<_>>()
    };
    let original_failures = failures(&app);
    let failure_text = app
        .transcript_cells
        .iter()
        .filter(|cell| cell.as_any().is::<history_cell::FailedTurnCell>())
        .map(|cell| lines_to_single_string(&cell.raw_lines()))
        .collect::<String>();
    assert_eq!(failure_text.matches(error.message.as_str()).count(), 3);
    assert!(failure_text.contains("Sep 6, 2000 at"));
    assert!(!failure_text.contains("Worked for"));
    assert_eq!(original_failures.len(), 3);
    assert_eq!(
        original_failures[0],
        ("turn-0".to_string(), oldest_completed_at, error.clone())
    );
    assert_eq!(
        original_failures[1],
        ("empty-failure".to_string(), None, error.clone())
    );
    let initial_cells = app.transcript_cells.clone();
    app.local_settings.tui.terminal_resize_reflow_max_rows = None;
    app.open_transcript_overlay(&mut tui);
    let mut overlapping = None;
    while app_server.has_older_history(thread_id) {
        let cursor = app_server
            .begin_older_history_page(thread_id)
            .expect("older page");
        let HistoryPage::Items(mut page) = cursor
            .load(
                &app_server.request_handle(),
                app_server.next_request_id(),
                thread_id,
                1,
            )
            .await?
        else {
            panic!("item frontier");
        };
        if let Some(item) = overlapping.take() {
            page.data.insert(0, item);
        }
        overlapping = page.data.last().cloned();
        app.handle_older_history_page(
            &mut tui,
            &mut app_server,
            thread_id,
            &cursor,
            Ok(HistoryPage::Items(page)),
        )
        .await?;
        assert_eq!(failures(&app), original_failures);
    }
    // Repeated messages retain distinct turn identities and the same immutable cells.
    for original in &initial_cells {
        if original.as_any().is::<history_cell::FailedTurnCell>() {
            assert!(
                app.transcript_cells
                    .iter()
                    .any(|cell| Arc::ptr_eq(cell, original))
            );
        }
    }
    let chronology = |app: &App| {
        app.transcript_cells
            .iter()
            .filter_map(|cell| {
                if let Some(failure) = cell.as_any().downcast_ref::<history_cell::FailedTurnCell>()
                {
                    Some(failure.turn_id.clone())
                } else {
                    let text = lines_to_single_string(&cell.raw_lines());
                    [
                        "Oldest answer",
                        "Middle answer",
                        "Newer answer",
                        "Newest answer",
                    ]
                    .into_iter()
                    .find(|answer| text.contains(*answer))
                    .map(str::to_string)
                }
            })
            .collect::<Vec<_>>()
    };
    let ordered = chronology(&app);
    assert_eq!(
        ordered,
        vec![
            "Oldest answer",
            "turn-0",
            "empty-failure",
            "Middle answer",
            "Newer answer",
            "turn-2",
            "Newest answer",
        ]
    );
    let restored_turns = app.thread_event_channels[&thread_id]
        .store
        .lock()
        .await
        .turns
        .clone();
    app.overlay = None;
    app.transcript_cells.clear();
    app.chat_widget.handle_thread_session(session);
    app.chat_widget
        .replay_thread_turns(restored_turns, ReplayKind::ThreadSnapshot);
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            app.insert_retained_history_cell(&mut tui, Arc::from(cell));
        }
    }
    assert_eq!(chronology(&app), ordered);
    assert_eq!(failures(&app), original_failures);
    app_server.shutdown().await?;
    proxy.await??;
    Ok(())
}

#[tokio::test]
async fn beginning_navigation_holds_the_view_until_the_last_page_arrives() -> Result<()> {
    for initial_scroll in [0, -1] {
        let (mut app, _codex_home, thread_id) =
            completed_history_app(&["Oldest", "Middle", "Newest"]).await?;
        let (mut app_server, _requests, proxy) = start_recording_app_server(
            &app.config,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
        )
        .await?;
        let started = app_server
            .resume_thread(
                &app.local_settings,
                app.config.clone(),
                thread_id,
                crate::app_server_session::ResumeModelSettings::RestoreFromThread,
            )
            .await?;
        app.transcript_cells = crate::thread_transcript::thread_items_to_transcript_cells(
            Some(thread_id),
            &app.config.cwd,
            started.turns.iter().flat_map(Turn::item_payloads).cloned(),
            crate::thread_transcript::RawReasoningVisibility::Hidden,
            Some(&app.config),
            &|_| crate::multi_agents::AgentMetadata::default(),
            crate::history_cell::ImagePathOrigin::Local,
        );
        // This fixture projects cells directly rather than dispatching replay notifications.
        for turn in &started.turns {
            if let Some(id) = crate::transcript_view::dialogue::final_response(turn) {
                app.transcript_view.record_final_response(id.to_string());
            }
        }
        app.enqueue_primary_thread_session(started.session, started.turns)
            .await?;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        app.app_event_tx = crate::app_event_sender::AppEventSender::new(event_tx);
        app.scrollback_has_older_history = app_server.has_older_history(thread_id);
        app.transcript_view.history = TranscriptHistoryState::Partial;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        tui.set_owned_screen(/*owned*/ true)?;
        let size = ratatui::layout::Size::new(/*width*/ 80, /*height*/ 18);
        let render = |app: &mut App, tui: &mut tui::Tui| -> Result<Buffer> {
            tui.screen_size_for_event(&TuiEvent::Resize(size))?;
            let bottom = app.render_owned_transcript(tui, size)?;
            let area = Rect::new(
                /*x*/ 0,
                /*y*/ 2, // The screen help and its separator are not transcript rows.
                size.width,
                bottom.y.saturating_sub(/*rhs*/ 2),
            );
            let mut buffer = Buffer::empty(area);
            let rendered =
                crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal);
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    buffer[(x, y)] = rendered[(x, y)].clone();
                }
            }
            Ok(buffer)
        };
        render(&mut app, &mut tui)?;
        if initial_scroll != 0 {
            app.transcript_view
                .scroll(&app.transcript_cells, initial_scroll);
        }
        let before = render(&mut app, &mut tui)?;
        // Keep real content inside the comparison region, not just a blank composer gap.
        assert!(
            before
                .content()
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .contains("Newest answer")
        );

        // Hold a small first page so the test can inspect each request/completion boundary.
        let cursor = app_server
            .begin_older_history_page(thread_id)
            .expect("older page");
        let HistoryPage::Items(first_page) = cursor
            .load(
                &app_server.request_handle(),
                app_server.next_request_id(),
                thread_id,
                1,
            )
            .await?
        else {
            panic!("item frontier");
        };
        let final_cursor =
            HistoryPage::Items(first_page.next_cursor.clone().expect("another older page"));
        tui.screen_size_for_event(&TuiEvent::Resize(size))?;
        assert!(app.handle_owned_transcript_event(
            &mut tui,
            &mut app_server,
            &TuiEvent::Key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::ALT)),
        )?);
        assert_eq!(
            (render(&mut app, &mut tui)?, app.transcript_view.history),
            (before.clone(), TranscriptHistoryState::LoadingBeginning),
        );
        app.handle_older_history_page(
            &mut tui,
            &mut app_server,
            thread_id,
            &cursor,
            Ok(HistoryPage::Items(first_page)),
        )
        .await?;
        assert!(app_server.is_older_history_page_pending(thread_id, &final_cursor));
        let final_event = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
            while let Some(event) = event_rx.recv().await {
                if matches!(event, AppEvent::OlderThreadHistoryLoaded { .. }) {
                    return Some(event);
                }
            }
            None
        })
        .await?
        .expect("final page completion");
        assert!(matches!(
            &final_event,
            AppEvent::OlderThreadHistoryLoaded { result: Ok(HistoryPage::Items(page)), .. }
                if page.next_cursor.is_none()
        ));
        // A draw while the completion is waiting must leave the viewport and loading intent intact.
        app.handle_owned_transcript_event(&mut tui, &mut app_server, &TuiEvent::Draw)?;
        assert_eq!(
            (render(&mut app, &mut tui)?, app.transcript_view.history),
            (before, TranscriptHistoryState::LoadingBeginning),
        );
        Box::pin(app.handle_event(&mut tui, &mut app_server, final_event)).await?;
        let final_buffer = render(&mut app, &mut tui)?;
        let visible = final_buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(visible.contains("Oldest prompt"), "visible: {visible}");
        assert_eq!(
            (
                app.transcript_view.history,
                app.transcript_view.is_following(),
                app_server.has_older_history(thread_id),
            ),
            (TranscriptHistoryState::Complete, false, false),
        );
        tui.set_owned_screen(/*owned*/ false)?;
        app_server.shutdown().await?;
        proxy.await??;
    }
    Ok(())
}

#[tokio::test]
async fn returning_to_latest_retains_pending_pages_without_continuing_to_the_beginning()
-> Result<()> {
    let (mut app, _codex_home, thread_id) =
        completed_history_app(&["Oldest", "Middle", "Newest"]).await?;
    let (mut app_server, _requests, proxy) = start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let started = app_server
        .resume_thread(
            &app.local_settings,
            app.config.clone(),
            thread_id,
            crate::app_server_session::ResumeModelSettings::RestoreFromThread,
        )
        .await?;
    app.transcript_cells = crate::thread_transcript::thread_items_to_transcript_cells(
        Some(thread_id),
        &app.config.cwd,
        started.turns.iter().flat_map(Turn::item_payloads).cloned(),
        crate::thread_transcript::RawReasoningVisibility::Hidden,
        Some(&app.config),
        &|_| crate::multi_agents::AgentMetadata::default(),
        crate::history_cell::ImagePathOrigin::Local,
    );
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    app.scrollback_has_older_history = app_server.has_older_history(thread_id);
    app.transcript_view.history = TranscriptHistoryState::Partial;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.set_owned_screen(/*owned*/ true)?;

    for (limit, expected_history) in [
        (1, TranscriptHistoryState::Partial),
        (100, TranscriptHistoryState::Complete),
    ] {
        app.transcript_view.handle_key(
            KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL),
            &app.transcript_cells,
        );
        let cursor = app_server
            .begin_older_history_page(thread_id)
            .expect("older page");
        let page = cursor
            .load(
                &app_server.request_handle(),
                app_server.next_request_id(),
                thread_id,
                limit,
            )
            .await?;
        let HistoryPage::Items(item_page) = &page else {
            panic!("item page");
        };
        let next_cursor = item_page.next_cursor.clone().map(HistoryPage::Items);
        let before = app.transcript_cells.len();
        app.transcript_view.handle_key(
            KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL),
            &app.transcript_cells,
        );
        app.handle_older_history_page(&mut tui, &mut app_server, thread_id, &cursor, Ok(page))
            .await?;
        assert!(
            app.transcript_cells.len() > before,
            "received history is retained"
        );
        assert_eq!(
            (
                app.transcript_view.history,
                app.transcript_view.is_following()
            ),
            (expected_history, true),
        );
        if let Some(next_cursor) = next_cursor {
            assert!(!app_server.is_older_history_page_pending(thread_id, &next_cursor));
        }
    }
    tui.set_owned_screen(/*owned*/ false)?;
    app_server.shutdown().await?;
    proxy.await??;
    Ok(())
}

#[tokio::test]
async fn stale_history_completions_preserve_the_current_request_and_failure_can_retry() -> Result<()>
{
    let (mut app, _codex_home, thread_id) =
        completed_history_app(&["Oldest", "Middle", "Newest"]).await?;
    let (mut app_server, _requests, proxy) = start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let started = app_server
        .resume_thread(
            &app.local_settings,
            app.config.clone(),
            thread_id,
            crate::app_server_session::ResumeModelSettings::RestoreFromThread,
        )
        .await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    app.scrollback_has_older_history = app_server.has_older_history(thread_id);
    app.transcript_view.history = TranscriptHistoryState::LoadingBeginning;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.set_owned_screen(/*owned*/ true)?;
    let cursor = app_server
        .begin_older_history_page(thread_id)
        .expect("older page");
    let page = cursor
        .load(
            &app_server.request_handle(),
            app_server.next_request_id(),
            thread_id,
            1,
        )
        .await?;
    for result in [Ok(page.clone()), Err("obsolete request failed".to_string())] {
        app.handle_older_history_page(
            &mut tui,
            &mut app_server,
            thread_id,
            &HistoryPage::Items("obsolete-cursor".to_string()),
            result,
        )
        .await?;
        assert_eq!(
            (
                app.transcript_cells.len(),
                app.transcript_view.history,
                app_server.is_older_history_page_pending(thread_id, &cursor),
            ),
            (0, TranscriptHistoryState::LoadingBeginning, true),
        );
    }
    let failure = AppEvent::OlderThreadHistoryLoaded {
        thread_id,
        cursor: cursor.clone(),
        result: Err("current request failed".to_string()),
    };
    Box::pin(app.handle_event(&mut tui, &mut app_server, failure)).await?;
    app.handle_owned_transcript_event(&mut tui, &mut app_server, &TuiEvent::Draw)?;
    assert_eq!(
        (
            app.transcript_view.history,
            app_server.is_older_history_page_pending(thread_id, &cursor),
        ),
        (TranscriptHistoryState::Failed, false),
    );
    assert_eq!(
        app_server.begin_older_history_page(thread_id),
        Some(cursor.clone())
    );
    app_server.cancel_older_history_page(
        thread_id,
        &HistoryPage::Items("obsolete-cursor".to_string()),
    );
    assert!(app_server.is_older_history_page_pending(thread_id, &cursor));
    app.transcript_view.history = TranscriptHistoryState::LoadingOlder;
    // Initialize the viewport as the app's first draw does before accepting Find input.
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 1,
    );
    let mut buffer = Buffer::empty(area);
    app.transcript_view
        .render(area, &mut buffer, &app.transcript_cells);
    app.transcript_view
        .jump_to_entry(&app.transcript_cells, /*index*/ 0);
    app.transcript_view.begin_search();
    app.transcript_view.paste_search("Middle answer");
    app.transcript_view.advance_search(&app.transcript_cells);
    let HistoryPage::Items(item_page) = &page else {
        panic!("item page");
    };
    let next_cursor =
        HistoryPage::Items(item_page.next_cursor.clone().expect("another older page"));
    app.handle_older_history_page(&mut tui, &mut app_server, thread_id, &cursor, Ok(page))
        .await?;
    assert_eq!(app.transcript_view.history, TranscriptHistoryState::Partial);
    assert!(!app.transcript_cells.is_empty());
    // The received page must be searched before another page can start, even near the top.
    assert!(!app_server.is_older_history_page_pending(thread_id, &next_cursor));
    // Scanning yields after the completion footer before reaching the answer in the same page.
    for _ in 0..8 {
        if !app.transcript_view.advance_search(&app.transcript_cells) {
            break;
        }
    }
    app.transcript_view
        .render(area, &mut buffer, &app.transcript_cells);
    let rendered = buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    assert!(rendered.contains("Middle answer"), "rendered: {rendered:?}",);

    assert_eq!(
        app_server.begin_older_history_page(thread_id),
        Some(next_cursor.clone())
    );
    app.handle_older_history_page(
        &mut tui,
        &mut app_server,
        thread_id,
        &cursor,
        Err("previous page failed late".to_string()),
    )
    .await?;
    assert!(app_server.is_older_history_page_pending(thread_id, &next_cursor));
    let history = app.transcript_view.history;
    let cell_count = app.transcript_cells.len();
    app.chat_widget.handle_thread_session(test_thread_session(
        ThreadId::new(),
        app.config.cwd.to_path_buf(),
    ));
    app.handle_older_history_page(
        &mut tui,
        &mut app_server,
        thread_id,
        &next_cursor,
        Err("previous thread failed late".to_string()),
    )
    .await?;
    assert_eq!(
        (
            app.transcript_view.history,
            app.transcript_cells.len(),
            app_server.is_older_history_page_pending(thread_id, &next_cursor),
        ),
        (history, cell_count, false),
    );
    tui.set_owned_screen(/*owned*/ false)?;
    app_server.shutdown().await?;
    proxy.await??;
    Ok(())
}
