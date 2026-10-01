//! Selection keys remain inside the overlay while drafts and backtrack previews are pending.

use super::*;
use crate::chatwidget::UserMessage;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn displaced_tool_calls_complete_once_and_disclosure_navigation_rejoins_current_history()
-> Result<()> {
    use crate::history_cell::ComputerActivityCell;
    use crate::history_cell::McpInvocation;
    use crate::history_cell::ToolOutputCell;
    use codex_protocol::models::FunctionCallOutputBody;
    use std::time::Duration;

    let mut app = make_test_app().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.transcript_cells.clear();
    let call = |id: &str, server: &str| {
        history_cell::new_active_mcp_tool_call(
            id.into(),
            McpInvocation {
                server: server.into(),
                tool: "inspect".into(),
                arguments: Some(json!({"path": "same"})),
            },
            /*animations_enabled*/ false,
        )
    };
    // Concurrent starts displace running calls into immutable history. Returned batches can
    // already own those calls when their completions arrive out of order.
    for id in ["first", "second"] {
        app.insert_history_cell(&mut tui, Box::new(call(id, "tools")));
    }
    app.insert_history_cell(
        &mut tui,
        Box::new(
            ToolOutputCell::from_output(
                "batch",
                /*call_id*/ None,
                "exec",
                /*namespace*/ None,
                &FunctionCallOutputBody::Text("model-visible batch".into()),
            )
            .unwrap(),
        ),
    );
    app.flush_native_history(&mut tui);
    assert!(tui.pending_history_lines_for_test().is_empty());
    for _ in 0..2 {
        app.transcript_view.record_tool_output_tokens(
            "tool-output-tokens:batch".into(),
            "outer-exec".into(),
            Some(128),
        );
    }
    let render = |app: &mut App| {
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 12,
        );
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        app.transcript_view
            .render(area, &mut buffer, &app.transcript_cells);
        buffer
    };
    render(&mut app);
    for (kind, column) in [
        (MouseEventKind::Down(MouseButton::Left), 4),
        (MouseEventKind::Drag(MouseButton::Left), 12),
        (MouseEventKind::Up(MouseButton::Left), 12),
    ] {
        app.transcript_view.handle_mouse(
            MouseEvent {
                kind,
                column,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            &app.transcript_cells,
        );
    }
    let selected = render(&mut app);
    for id in ["second", "first"] {
        let mut completed = call(id, "tools");
        completed.complete(
            Duration::from_millis(/*millis*/ 1),
            Ok(codex_protocol::mcp::CallToolResult {
                content: vec![json!({"type": "text", "text": "raw nested result"})],
                structured_content: None,
                is_error: Some(false),
                meta: None,
            }),
        );
        app.insert_history_cell(&mut tui, Box::new(completed));
    }
    assert_eq!(app.transcript_cells.len(), 1);
    assert_eq!(
        app.transcript_cells[0].activity_ids(),
        vec!["mcp:first", "mcp:second", "output:tool-output-tokens:batch"]
    );
    app.flush_native_history(&mut tui);
    let native = tui
        .pending_history_lines_for_test()
        .iter()
        .map(|line| line.line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!native.contains("Calling"));
    assert_eq!(native.matches("Called").count(), 2);
    // A selection keeps its source revision, but never also displays the completion alias.
    assert_eq!(render(&mut app), selected);
    app.transcript_view.handle_key(
        KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE),
        &app.transcript_cells,
    );
    let completed = render(&mut app);
    let text = completed
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    assert!(!text.contains("Calling"));
    assert_eq!(text.matches("Called").count(), 2);
    assert_eq!(text.matches("128 tokens").count(), 1);

    // A flushed computer group must preserve its other running calls and its own details.
    let mut computer = ComputerActivityCell::default();
    computer.start(call("computer-first", "cua_repl"));
    computer.start(call("computer-second", "cua_repl"));
    app.insert_history_cell(&mut tui, Box::new(computer));
    let mut computer = ComputerActivityCell::default();
    computer.start(call("computer-new", "cua_repl"));
    let mut previous = call("computer-second", "cua_repl");
    previous.mark_failed();
    computer.start(previous);
    app.insert_history_cell(&mut tui, Box::new(computer));
    assert_eq!(app.transcript_cells.len(), 3);
    assert_eq!(
        app.transcript_cells[1].activity_ids(),
        vec!["mcp:computer-first", "mcp:computer-second"]
    );
    assert_eq!(
        app.transcript_cells[2].activity_ids(),
        vec!["mcp:computer-new"]
    );
    for query in ["running", "finished"] {
        app.insert_history_cell(
            &mut tui,
            Box::new(if query == "running" {
                history_cell::new_active_web_search_call(
                    "web".into(),
                    query.into(),
                    /*animations_enabled*/ false,
                )
            } else {
                history_cell::new_web_search_call(
                    "web".into(),
                    query.into(),
                    codex_app_server_protocol::WebSearchAction::Other,
                )
            }),
        );
    }
    assert_eq!(app.transcript_cells.len(), 4);
    insta::assert_snapshot!(format!("completed batch\n{completed:?}"));
    let exec = |id: &str| {
        let command = vec!["cat".into(), "same-file".into()];
        let parsed = codex_shell_command::parse_command::parse_command(&command);
        crate::exec_cell::new_active_exec_command(
            id.into(),
            command,
            parsed,
            codex_app_server_protocol::CommandExecutionSource::Agent,
            /*interaction_input*/ None,
            /*animations_enabled*/ false,
        )
    };
    let mut pending = exec("exec-first");
    pending
        .group
        .calls
        .push(exec("exec-second").group.calls.remove(/*index*/ 0));
    app.insert_history_cell(&mut tui, Box::new(pending));
    let mut completed = exec("exec-second");
    completed.complete_call(
        "exec-second",
        crate::exec_cell::CommandOutput::new(/*exit_code*/ 0, "command output".into()),
        Duration::from_millis(/*millis*/ 1),
    );
    app.insert_history_cell(&mut tui, Box::new(completed));
    assert_eq!(app.transcript_cells.len(), 5);
    assert_eq!(
        app.transcript_cells[4].activity_ids(),
        vec!["exec:exec-first", "exec:exec-second"]
    );
    Ok(())
}

#[tokio::test]
async fn text_selection_temporarily_replaces_the_rendered_backtrack_highlight() -> Result<()> {
    let (mut app, mut events, mut operations) = make_test_app_with_channels().await;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.chat_widget.apply_external_edit("retained draft".into());
    app.chat_widget.handle_key_event(KeyCode::Left.into());
    let draft = app.chat_widget.capture_thread_input_state();
    let prompt = "Selected prompt";
    app.transcript_cells = vec![Arc::new(UserHistoryCell {
        message: prompt.into(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
        spoken: false,
    })];
    app.open_transcript_overlay(&mut tui);
    while events.try_recv().is_ok() {}
    while operations.try_recv().is_ok() {}
    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        TuiEvent::Key(KeyCode::Esc.into()),
    )
    .await?;
    assert!(app.backtrack.overlay_preview_active);
    let preview = (app.backtrack.base_id, app.backtrack.nth_user_message);
    let render = |app: &mut App| {
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 32, /*height*/ 12,
        );
        let mut buffer = Buffer::empty(area);
        // Match the App owner's footer projection before this fixed-size overlay render.
        let footer = app.prompt_navigation_footer(area.width);
        let Some(Overlay::Transcript(overlay)) = app.overlay.as_mut() else {
            panic!("expected transcript overlay");
        };
        overlay.browsing_footer = footer.and_then(|footer| footer.text.lines.into_iter().next());
        overlay.render(area, &mut buffer);
        let highlighted = buffer
            .content
            .iter()
            .filter(|cell| cell.modifier.contains(ratatui::style::Modifier::REVERSED))
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        (buffer, highlighted)
    };
    let (backtrack, highlighted) = render(&mut app);
    assert_eq!(highlighted, prompt);

    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
    )
    .await?;
    let (_, highlighted) = render(&mut app);
    assert_eq!(highlighted, "");
    // The selection starts on the leading blank source row before the user message.
    for _ in 0..2 {
        app.handle_tui_event(
            &mut tui,
            &mut app_server,
            TuiEvent::Key(KeyCode::Right.into()),
        )
        .await?;
    }
    let (_, highlighted) = render(&mut app);
    // The selected leading newline does not paint an empty cell.
    assert_eq!(highlighted, "S");

    app.handle_tui_event(
        &mut tui,
        &mut app_server,
        TuiEvent::Key(KeyCode::Esc.into()),
    )
    .await?;
    let (restored, highlighted) = render(&mut app);
    assert_eq!(highlighted, prompt);
    assert_eq!(restored, backtrack);
    assert!(app.backtrack.overlay_preview_active);
    assert_eq!(
        (app.backtrack.base_id, app.backtrack.nth_user_message),
        preview
    );
    assert_eq!(app.chat_widget.capture_thread_input_state(), draft);
    assert!(operations.try_recv().is_err());
    assert!(events.try_recv().is_err());
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn offline_selection_reserves_keys_and_preserves_draft_and_backtrack_state() -> Result<()> {
    let (mut app, mut events, mut operations) = make_test_app_with_channels().await;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.keymap = RuntimeKeymap::from_config(&toml::from_str(
        "[pager]\nclose = 'ctrl-c x'\npage_up = 'ctrl-x p'\n",
    )?)
    .unwrap();
    app.chat_widget
        .restore_user_message_to_composer(UserMessage {
            text: "draft prompt".to_owned(),
            local_images: Vec::new(),
            remote_image_urls: vec!["https://example.com/draft.png".to_owned()],
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        });
    app.chat_widget.handle_key_event(KeyCode::Left.into());
    let draft = app.chat_widget.capture_thread_input_state();
    app.transcript_cells = vec![plain_line_cell("selected text")];
    app.open_transcript_overlay(&mut tui);
    let Some(Overlay::Transcript(overlay)) = app.overlay.as_mut() else {
        panic!("transcript overlay");
    };
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 12,
    );
    overlay.render(area, &mut Buffer::empty(area));
    app.backtrack.overlay_preview_active = true;
    app.backtrack.nth_user_message = 3;
    while events.try_recv().is_ok() {}
    while operations.try_recv().is_ok() {}

    assert_eq!(
        app.route_key_chord_event(
            &mut tui,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        ),
        None,
    );
    assert!(app.key_chord_matcher.is_pending());
    app.reconnect.offline = true;
    for key in [
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        KeyCode::Enter.into(),
        KeyCode::Right.into(),
        KeyCode::Left.into(),
        KeyCode::Esc.into(),
    ] {
        assert!(matches!(
            app.handle_tui_event(&mut tui, &mut app_server, TuiEvent::Key(key))
                .await?,
            AppRunControl::Continue,
        ));
        assert!(app.overlay.is_some());
        assert!(!app.key_chord_matcher.is_pending());
        assert_eq!(app.chat_widget.capture_thread_input_state(), draft);
        assert_eq!(app.backtrack.nth_user_message, 3);
        assert!(app.backtrack.overlay_preview_active);
        assert!(operations.try_recv().is_err());
        assert!(events.try_recv().is_err());
    }
    let Some(Overlay::Transcript(overlay)) = app.overlay.as_ref() else {
        panic!("transcript overlay");
    };
    assert!(!overlay.has_active_interaction());
    app.chat_widget.insert_str("!");
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "draft promp!t"
    );
    assert!(matches!(
        app.handle_tui_event(
            &mut tui,
            &mut app_server,
            TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        )
        .await?,
        AppRunControl::Exit(ExitReason::UserRequested),
    ));
    app_server.shutdown().await?;
    Ok(())
}
