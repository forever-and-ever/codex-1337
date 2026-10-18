use std::sync::Arc;
use std::sync::Mutex;

use itertools::Itertools;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::history_cell::HistoryCell;
use crate::transcript_view::Anchor;
use crate::transcript_view::EntryKey;
use crate::transcript_view::Position;
use crate::transcript_view::TranscriptView;
use crate::transcript_view::tests::cell;
use crate::transcript_view::tests::render;

#[derive(Debug)]
struct GatedCell(Arc<Mutex<()>>);

impl HistoryCell for GatedCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let _gate = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        vec!["older".into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(24)
    }
}

#[tokio::test]
async fn history_prefetch_waits_for_prepared_rows() -> Result<(), Box<dyn std::error::Error>> {
    let gate = Arc::new(Mutex::new(()));
    let cells = vec![
        Arc::new(GatedCell(Arc::clone(&gate))) as Arc<dyn HistoryCell>,
        cell("current"),
    ];
    let mut view = TranscriptView {
        area: Rect::new(0, 0, 24, 10),
        position: Position::Reading(Anchor {
            key: EntryKey::cell(&cells[1]),
            index: 1,
            offset: 0,
            row_bias: 0,
        }),
        ..Default::default()
    };
    // Only the current entry is ready. An unfinished older layout is not an empty entry.
    assert!(view.layout(&cells, 1).is_some());
    let (draw_tx, mut draw_rx) = tokio::sync::broadcast::channel(16);
    {
        let _held = gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        view.cache.frames = Some(crate::tui::FrameRequester::new(draw_tx));
        assert!(!view.needs_history(&cells));

        view.cache.clear();
        view.cache.begin_frame();
        view.position = Position::Latest;
        assert!(!view.needs_history(&cells));
    }

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            render(&mut view, &cells, 24, 10);
            if view.needs_history(&cells) {
                return Ok::<(), tokio::sync::broadcast::error::RecvError>(());
            }
            draw_rx.recv().await?;
        }
    })
    .await??;
    Ok(())
}

#[test]
fn live_preparation_does_not_freeze_retained_navigation() {
    use crate::chatwidget::ActiveCellTranscriptKey;
    use crate::transcript_view::live_preparation::OwnedLiveTail;

    let cells = vec![cell((0..16).map(|row| format!("row {row}")).join("\n"))];
    let mut extended = cells.clone();
    extended.push(cell("new submitted message"));
    let mut view = TranscriptView::default();
    render(&mut view, &extended, 32, 4);
    render(&mut view, &cells, 32, 4);
    view.cache.frames = Some(crate::tui::FrameRequester::test_dummy());
    let before = render(&mut view, &cells, 32, 4);

    let gate = Arc::new(Mutex::new(()));
    let _held = gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    view.sync_owned_live_tail(
        32,
        Some(ActiveCellTranscriptKey {
            cacheable: true,
            revision: 1,
            is_stream_continuation: false,
            is_tool_entry: false,
            animation_tick: None,
        }),
        false,
        || OwnedLiveTail {
            active: Some(Arc::new(GatedCell(Arc::clone(&gate)))),
            auxiliary: Vec::new(),
        },
    );

    // A new live revision must not freeze navigation through already prepared content.
    view.scroll(&cells, -2);
    assert_ne!(render(&mut view, &cells, 32, 4), before);
    view.jump_to_latest();
    render(&mut view, &extended, 32, 4);
    assert!(
        view.visible
            .iter()
            .any(|row| row.key == EntryKey::cell(&extended[1]))
    );
}

#[tokio::test]
async fn return_to_latest_cancels_unfinished_history_scroll()
-> Result<(), Box<dyn std::error::Error>> {
    let gate = Arc::new(Mutex::new(()));
    let cells = vec![
        Arc::new(GatedCell(Arc::clone(&gate))) as Arc<dyn HistoryCell>,
        cell((0..16).map(|row| format!("row {row}")).join("\n")),
    ];
    let mut view = TranscriptView::default();
    let before = render(&mut view, &cells, 32, 4);
    let (draw_tx, mut draw_rx) = tokio::sync::broadcast::channel(16);
    {
        let _held = gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        view.cache.frames = Some(crate::tui::FrameRequester::new(draw_tx));
        view.scroll(&cells, -100);
        assert!(view.deferred_scroll.is_some());
        view.jump_to_latest();
        assert_eq!(render(&mut view, &cells, 32, 4), before);
    }

    // Finishing a superseded scroll must not undo a later return to the live edge.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            render(&mut view, &cells, 32, 4);
            if view.deferred_scroll.is_none() && !view.cache.waiting {
                return Ok::<(), tokio::sync::broadcast::error::RecvError>(());
            }
            draw_rx.recv().await?;
        }
    })
    .await??;
    assert!(view.is_following());
    assert_eq!(render(&mut view, &cells, 32, 4), before);
    Ok(())
}

#[tokio::test]
async fn history_layouts_finish_across_displayed_and_current_revisions()
-> Result<(), Box<dyn std::error::Error>> {
    let cells: Vec<Arc<dyn HistoryCell>> =
        vec![Arc::new(crate::history_cell::AgentMarkdownCell::new(
            (0..16)
                .map(|row| format!("retained response {row}"))
                .join("\n"),
            std::path::Path::new("/tmp"),
        ))];
    let mut extended = vec![cell("older history")];
    extended.extend(cells.iter().cloned());
    let mut view = TranscriptView {
        area: Rect::new(0, 0, 40, 8),
        ..Default::default()
    };
    let (draw_tx, mut draw_rx) = tokio::sync::broadcast::channel(16);
    view.cache.frames = Some(crate::tui::FrameRequester::new(draw_tx));
    // Navigation uses displayed cells while a loaded page prepares the current cells.
    // Switching sources must not invalidate either source's unfinished layout.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            view.cache.begin_frame();
            let retained = view.layout(&cells, 0);
            let current = view.layout(&extended, 1);
            if retained.is_some() && current.is_some() {
                return Ok::<(), tokio::sync::broadcast::error::RecvError>(());
            }
            draw_rx.recv().await?;
        }
    })
    .await??;
    Ok(())
}
