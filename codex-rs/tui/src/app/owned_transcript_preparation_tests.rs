//! Prepared stream completions retain item order and immutable presented rows.

use super::*;
use pretty_assertions::assert_eq;

pub(super) async fn out_of_order_streams(
    app: &mut App,
    tui: &mut tui::Tui,
    area: Rect,
    retained: Arc<dyn HistoryCell>,
) -> Result<()> {
    // Two finalizations may complete out of order after unrelated output arrived.
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);
    let make_final = |id: &str| crate::streaming::completion::PreparedStream {
        kind: crate::streaming::completion::RevisionKind::Final(None),
        thread_id,
        item_id: id.into(),
        cell: Arc::new(history_cell::AgentMarkdownCell::new(
            format!("Final {id}"),
            &app.config.cwd,
        )),
    };
    let first = make_final("first");
    let second = make_final("second");
    let anchor: Arc<dyn HistoryCell> = Arc::new(history_cell::StreamAnchorCell {
        item_id: "first".into(),
    });
    assert!(anchor.raw_lines().is_empty());
    assert!(anchor.transcript_lines(40).is_empty());
    assert!(anchor.copy_source().is_none());
    assert!(anchor.activity_ids().is_empty());
    assert!(anchor.activity_disclosure(40).is_none());
    let before_anchor = app.transcript_cells.clone();
    let mut without_anchor = Buffer::empty(area);
    app.transcript_view.jump_to_latest();
    app.transcript_view
        .render(area, &mut without_anchor, &before_anchor);
    app.transcript_cells.push(anchor);
    let mut with_anchor = Buffer::empty(area);
    app.transcript_view
        .render(area, &mut with_anchor, &app.transcript_cells);
    assert_eq!(
        with_anchor, without_anchor,
        "the anchor must not add rows, headers or spacing"
    );
    let mut fragment = history_cell::AgentMessageCell::new(vec!["first streamed".into()], true);
    fragment.item_id = Some("first".into());
    app.transcript_cells.extend([
        Arc::new(fragment) as Arc<dyn HistoryCell>,
        Arc::new(history_cell::StreamAnchorCell {
            item_id: "second".into(),
        }),
        retained.clone(),
    ]);
    let keep = app.transcript_cells.last().unwrap().clone();
    let stream_rows = |width| {
        Arc::new(history_cell::StreamRowsCell {
            mode: HistoryRenderMode::Rich,
            width: Some(width),
            reserved: 2,
            item_id: Some("first".into()),
            content: history_cell::StreamRowContent::Prepared(Box::new(
                history_cell::AgentMessageCell::new(vec!["provisional".into()], true),
            )),
        }) as Arc<dyn HistoryCell>
    };
    let old_rows = stream_rows(24);
    let mut stream_view = crate::transcript_view::TranscriptView::default();
    let old_area = Rect::new(0, 0, 26 + crate::transcript_geometry::GUTTER_COLUMNS, 3);
    let new_area = Rect {
        width: old_area.width + 1,
        ..old_area
    };
    let mut shown = Buffer::empty(old_area);
    stream_view.render(old_area, &mut shown, std::slice::from_ref(&old_rows));
    assert!(buffer_text(&shown).contains("provisional"));
    let mut blank = Buffer::empty(new_area);
    stream_view.render(new_area, &mut blank, std::slice::from_ref(&old_rows));
    assert!(
        buffer_text(&blank).trim().is_empty(),
        "old hard breaks must never be rewrapped at the new width"
    );
    let new_rows = stream_rows(25);
    let mut prepared_buffer = Buffer::empty(new_area);
    stream_view.render(
        new_area,
        &mut prepared_buffer,
        std::slice::from_ref(&new_rows),
    );
    assert!(buffer_text(&prepared_buffer).contains("provisional"));
    app.handle_prepared_stream(
        tui,
        crate::streaming::completion::PreparedStream {
            kind: crate::streaming::completion::RevisionKind::Rewrap,
            thread_id,
            item_id: "first".into(),
            cell: new_rows,
        },
    )?;
    assert!(app.transcript_cells.iter().any(|cell| {
        cell.as_any()
            .downcast_ref::<history_cell::StreamAnchorCell>()
            .is_some_and(|anchor| anchor.item_id == "first")
    }));
    app.handle_prepared_stream(tui, second)?;
    assert!(Arc::ptr_eq(app.transcript_cells.last().unwrap(), &keep));
    app.handle_prepared_stream(tui, first)?;
    assert!(
        !app.transcript_cells
            .iter()
            .any(|cell| cell.as_any().is::<history_cell::StreamAnchorCell>())
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
    let foreign = crate::streaming::completion::PreparedStream {
        kind: crate::streaming::completion::RevisionKind::Final(None),
        thread_id: ThreadId::new(),
        item_id: "first".into(),
        cell: retained.clone(),
    };
    app.handle_prepared_stream(tui, foreign)?;
    let missing = crate::streaming::completion::PreparedStream {
        kind: crate::streaming::completion::RevisionKind::Final(None),
        thread_id,
        item_id: "missing".into(),
        cell: retained.clone(),
    };
    app.handle_prepared_stream(tui, missing)?;
    assert!(
        app.transcript_cells
            .iter()
            .zip(&finished)
            .all(|(a, b)| Arc::ptr_eq(a, b))
    );
    app.transcript_cells
        .push(Arc::new(history_cell::StreamAnchorCell {
            item_id: "cancelled".into(),
        }));
    app.discard_stream_items(tui, &["cancelled".into()]);
    assert_eq!(app.transcript_cells.len(), finished.len());
    Ok(())
}
