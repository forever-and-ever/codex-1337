//! Input assertions are anchored to the presented revision, not unpainted cells.

use super::*;
use pretty_assertions::assert_eq;

pub(super) async fn select_transcript(
    app: &mut App,
    tui: &mut tui::Tui,
    app_server: &mut AppServerSession,
    detailed: bool,
) -> Result<()> {
    app.transcript_view = crate::transcript_view::TranscriptView::default();
    app.transcript_view
        .set_presentation(detailed, HistoryRenderMode::Rich);
    app.transcript_cells = vec![Arc::new(PlainHistoryCell::new(vec![
        "select this transcript".into(),
    ]))];
    app.render_owned_transcript(tui, Size::new(/*width*/ 80, /*height*/ 12))?;
    let presented =
        crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal).clone();
    // An unpresented revision must not replace the cells behind the retained hit areas.
    let presented_cells = std::mem::replace(
        &mut app.transcript_cells,
        vec![Arc::new(PlainHistoryCell::new(vec![
            "unpresented replacement".into(),
        ]))],
    );
    app.handle_tui_event(
        tui,
        app_server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
    )
    .await?;
    assert!(
        crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal)
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
            .contains("select this transcript")
    );
    for _ in 0..6 {
        app.handle_tui_event(
            tui,
            app_server,
            TuiEvent::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT)),
        )
        .await?;
    }
    // Key routing changes interaction state, not the already presented frame.
    assert_eq!(
        crate::custom_terminal::test_support::last_rendered_buffer(&tui.terminal),
        &presented,
    );
    assert_eq!(
        app.transcript_view
            .selected_text(&app.transcript_cells)
            .as_deref(),
        Some("select")
    );
    app.transcript_cells = presented_cells;
    Ok(())
}
