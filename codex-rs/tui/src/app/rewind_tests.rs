//! Rewind's contract: the shortcut opens it on the latest prompt, arrows move it, Escape leaves
//! without reverting, Enter reverts before the chosen prompt, and the selection holds its prompt
//! while older history arrives.

use std::sync::Arc;

use codex_protocol::ThreadId;
use color_eyre::eyre::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use ratatui::layout::Size;
use ratatui::style::Modifier;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::App;
use crate::app::owned_transcript::tests::attach_thread;
use crate::app::owned_transcript::tests::buffer_text;
use crate::app::owned_transcript::tests::user_cell;
use crate::app::tests::make_test_app_with_channels;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::custom_terminal::test_support::last_rendered_buffer;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::tui::Tui;
use crate::tui::TuiEvent;

const SIZE: Size = Size::new(/*width*/ 80, /*height*/ 14);
const REWIND: KeyEvent = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);

/// One key, then the rewind inputs it queued, then a frame: the live loop's order. Returns the
/// other events the key sent.
async fn press(
    app: &mut App,
    tui: &mut Tui,
    server: &mut AppServerSession,
    events: &mut UnboundedReceiver<AppEvent>,
    key: KeyEvent,
) -> Result<Vec<AppEvent>> {
    app.handle_tui_event(tui, server, TuiEvent::Key(key))
        .await?;
    let mut others = Vec::new();
    while let Ok(event) = events.try_recv() {
        if matches!(event, AppEvent::Rewind(_)) {
            Box::pin(app.handle_event(tui, server, event)).await?;
        } else {
            others.push(event);
        }
    }
    app.render_owned_transcript(tui, SIZE)?;
    Ok(others)
}

fn highlighted(tui: &Tui) -> String {
    last_rendered_buffer(&tui.terminal)
        .content
        .iter()
        .filter(|cell| cell.modifier.contains(Modifier::REVERSED))
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

fn answer(text: &str) -> Arc<dyn HistoryCell> {
    Arc::new(PlainHistoryCell::new(vec![text.to_owned().into()]))
}

#[tokio::test]
async fn rewind_moves_cancels_and_reverts_before_the_chosen_prompt() -> Result<()> {
    let (mut app, mut events, _operations) = make_test_app_with_channels().await;
    attach_thread(&mut app, ThreadId::new());
    app.transcript_cells = vec![
        user_cell("first question"),
        answer("first answer"),
        user_cell("second question"),
        user_cell("third question"),
    ];
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.set_owned_screen(/*owned*/ true)?;
    app.render_owned_transcript(&mut tui, SIZE)?;

    press(&mut app, &mut tui, &mut server, &mut events, REWIND).await?;
    assert_eq!(highlighted(&tui), "third question");
    press(
        &mut app,
        &mut tui,
        &mut server,
        &mut events,
        KeyCode::Up.into(),
    )
    .await?;
    assert_eq!(highlighted(&tui), "second question");
    insta::assert_snapshot!(
        "rewinding",
        buffer_text(last_rendered_buffer(&tui.terminal))
    );

    let others = press(
        &mut app,
        &mut tui,
        &mut server,
        &mut events,
        KeyCode::Esc.into(),
    )
    .await?;
    assert!(
        !others
            .iter()
            .any(|event| matches!(event, AppEvent::RevertSessionForPromptEdit { .. }))
    );
    assert_eq!(
        (
            highlighted(&tui),
            app.chat_widget.shows_rewind(),
            app.transcript_view.dialogue_only()
        ),
        (String::new(), false, true)
    );

    for key in [
        REWIND,
        KeyCode::Up.into(),
        KeyCode::Up.into(),
        KeyCode::Down.into(),
    ] {
        press(&mut app, &mut tui, &mut server, &mut events, key).await?;
    }
    let others = press(
        &mut app,
        &mut tui,
        &mut server,
        &mut events,
        KeyCode::Enter.into(),
    )
    .await?;
    let reverts: Vec<_> = others
        .iter()
        .filter_map(|event| match event {
            AppEvent::RevertSessionForPromptEdit {
                selected_cell,
                prompt,
                ..
            } => Some((
                Arc::ptr_eq(selected_cell, &app.transcript_cells[2]),
                prompt.text.as_str(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(reverts, [(true, "second question")]);
    assert!(app.rewind.is_none() && !app.chat_widget.shows_rewind());
    server.shutdown().await?;
    tui.set_owned_screen(/*owned*/ false)?;
    Ok(())
}

#[tokio::test]
async fn rewind_finds_a_prompt_in_older_history_and_keeps_it_through_pages() -> Result<()> {
    let (mut app, mut events, _operations) = make_test_app_with_channels().await;
    attach_thread(&mut app, ThreadId::new());
    app.scrollback_has_older_history = true;
    app.transcript_cells = vec![answer("recent answer")];
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.set_owned_screen(/*owned*/ true)?;
    app.render_owned_transcript(&mut tui, SIZE)?;

    press(&mut app, &mut tui, &mut server, &mut events, REWIND).await?;
    assert!(app.chat_widget.shows_rewind() && app.rewind_needs_history());

    let older = user_cell("older prompt");
    app.transcript_cells
        .splice(0..0, [Arc::clone(&older), user_cell("newer prompt")]);
    app.render_owned_transcript(&mut tui, SIZE)?;
    assert_eq!(highlighted(&tui), "newer prompt");
    assert!(!app.rewind_needs_history());
    press(
        &mut app,
        &mut tui,
        &mut server,
        &mut events,
        KeyCode::Up.into(),
    )
    .await?;
    assert!(app.rewind_needs_history());

    app.transcript_cells
        .splice(0..0, [user_cell("oldest prompt"), answer("oldest answer")]);
    app.render_owned_transcript(&mut tui, SIZE)?;
    assert_eq!(highlighted(&tui), "older prompt");

    app.transcript_cells
        .retain(|cell| !Arc::ptr_eq(cell, &older));
    app.render_owned_transcript(&mut tui, SIZE)?;
    assert!(app.rewind.is_none() && !app.chat_widget.shows_rewind());
    server.shutdown().await?;
    tui.set_owned_screen(/*owned*/ false)?;
    Ok(())
}
