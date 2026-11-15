//! Completed responses replace their markers without disturbing interleaved output.

use super::*;
use pretty_assertions::assert_eq;

pub(super) fn out_of_order_streams(
    app: &mut App,
    tui: &mut tui::Tui,
    area: Rect,
    retained: &Arc<dyn HistoryCell>,
) -> Result<()> {
    // Finalizations can complete out of order after unrelated output arrives.
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);
    let make_final = |app: &mut App, id: &str| {
        let prepared = app
            .chat_widget
            .admit_stream_preparation(thread_id, id.into());
        prepared.complete(
            None,
            Arc::new(history_cell::AgentMarkdownCell::new(
                format!("Final {id}"),
                &app.config.cwd,
            )),
        );
        prepared
    };
    let first = make_final(app, "first");
    let second = make_final(app, "second");
    let marker: Arc<dyn HistoryCell> = Arc::new(history_cell::RespondingCell {
        item_id: "first".into(),
    });
    assert!(marker.copy_source().is_none());
    assert_eq!(marker.activity_ids(), Vec::<String>::new());
    assert!(marker.activity_disclosure(40).is_none());
    app.transcript_cells.push(marker);
    let mut responding = Buffer::empty(area);
    app.transcript_view.jump_to_latest();
    app.transcript_view
        .render(area, &mut responding, &app.transcript_cells);
    assert!(buffer_text(&responding).contains("Responding"));
    app.transcript_cells.extend([
        Arc::new(history_cell::RespondingCell {
            item_id: "second".into(),
        }) as Arc<dyn HistoryCell>,
        retained.clone(),
    ]);
    let keep = app.transcript_cells.last().unwrap().clone();
    app.handle_prepared_stream(tui, &second)?;
    assert!(Arc::ptr_eq(app.transcript_cells.last().unwrap(), &keep));
    assert!(app.transcript_cells.iter().any(|cell| {
        cell.as_any()
            .downcast_ref::<history_cell::RespondingCell>()
            .is_some_and(|marker| marker.item_id == "first")
    }));
    app.handle_prepared_stream(tui, &first)?;
    assert!(
        !app.transcript_cells
            .iter()
            .any(|cell| cell.as_any().is::<history_cell::RespondingCell>())
    );
    let texts: Vec<_> = app
        .transcript_cells
        .iter()
        .filter_map(|cell| cell.copy_source())
        .collect();
    assert!(
        texts.iter().position(|text| *text == "Final first")
            < texts.iter().position(|text| *text == "Final second")
    );
    let finished = app.transcript_cells.clone();
    for (thread_id, item_id) in [(ThreadId::new(), "first"), (thread_id, "missing")] {
        let prepared =
            crate::streaming::completion::PreparedStream::pending(thread_id, item_id.into());
        prepared.complete(None, retained.clone());
        app.handle_prepared_stream(tui, &prepared)?;
    }
    assert!(
        app.transcript_cells
            .iter()
            .zip(&finished)
            .all(|(a, b)| Arc::ptr_eq(a, b))
    );
    app.transcript_cells
        .push(Arc::new(history_cell::RespondingCell {
            item_id: "cancelled".into(),
        }));
    app.discard_stream_items(tui, &["cancelled".into()]);
    assert_eq!(app.transcript_cells.len(), finished.len());
    Ok(())
}
