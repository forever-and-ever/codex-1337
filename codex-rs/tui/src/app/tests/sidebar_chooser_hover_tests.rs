#![cfg(test)]

use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn hovering_a_saved_chat_highlights_both_lines_without_moving_selection() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let parent = ThreadId::new();
    let child = ThreadId::new();
    app.chat_widget
        .handle_thread_session(test_thread_session(parent, app.config.cwd.to_path_buf()));
    app.chat_widget.replay_thread_turns(
        vec![test_turn("parent-turn", TurnStatus::Completed, Vec::new())],
        crate::chatwidget::ReplayKind::ResumeInitialMessages,
    );
    app.active_thread_id = Some(parent);
    app.primary_thread_id = Some(parent);
    let pane = app.sidebar_mut();
    pane.show(
        sidebar::SidebarContent::Chooser,
        sidebar::PaneFocus::Sidebar,
    );
    pane.update_saved_chats(vec![child.into()]);
    pane.latest_prompts.insert(child, "Hover preview".into());
    pane.choice = 0;
    let choice = pane.choice;
    enable_side_retention(&mut app)?;
    app.local_settings.tui.animations = false;

    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.set_owned_screen(/*owned*/ true)?;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let size = Size::new(/*width*/ 100, /*height*/ 18);
    let colors = crate::terminal_probe::DefaultColors {
        fg: (224, 224, 224),
        bg: (20, 20, 20),
    };
    let mut redraws = crate::tui::test_support::record_frame_requests(&mut tui);
    let frame = crate::terminal_palette::with_test_default_colors(Some(colors), || {
        side_frame(&mut app, &mut tui, size)
    })?;
    let heading = frame
        .lines()
        .position(|line| line.contains("Side chats  "))
        .expect("side chats heading is visible") as u16;
    let chooser = app
        .conversation_frame
        .chooser_area()
        .expect("saved chat chooser");
    let preview = frame
        .lines()
        .position(|line| line.contains("Hover preview"))
        .expect("saved chat preview is visible") as u16;
    let title = preview - 1;
    let column = crate::sidebar_chooser::body_row(chooser, title).right() - 1;
    let initial_fill = [title, preview].map(|y| tui.terminal.previous_buffer()[(column, y)].bg);
    let mut hover_bg = None;
    for (name, y, redraw) in [
        ("preview", preview, true),
        ("title", title, false),
        ("heading", heading, true),
    ] {
        while redraws.try_recv().is_ok() {}
        let event = mouse(MouseEventKind::Moved, column, y);
        boxed_future!(app.handle_tui_event(&mut tui, &mut app_server, event)).await?;
        assert_eq!(redraws.try_recv().is_ok(), redraw, "{name}");
        crate::terminal_palette::with_test_default_colors(Some(colors), || {
            side_frame(&mut app, &mut tui, size)
        })?;
        let buffer = tui.terminal.previous_buffer();
        let fill = [title, preview].map(|y| buffer[(column, y)].bg);
        if name == "heading" {
            assert_eq!(
                fill, initial_fill,
                "leaving the entry restores its original fill"
            );
        } else {
            assert_ne!(fill[0], initial_fill[0], "hover highlights the title");
            assert_ne!(fill[1], initial_fill[1], "hover highlights the preview");
            assert_eq!(fill, [*hover_bg.get_or_insert(fill[0]); 2], "{name}");
        }
        assert_eq!(app.sidebar().unwrap().choice, choice);
    }
    app_server.shutdown().await?;
    Ok(())
}
