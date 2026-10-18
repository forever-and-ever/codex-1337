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
            let _ = render(&mut view, &cells, 24, 10);
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
    let _ = render(&mut view, &extended, 32, 4);
    let _ = render(&mut view, &cells, 32, 4);
    view.cache.frames = Some(crate::tui::FrameRequester::test_dummy());
    let before = render(&mut view, &cells, 32, 4);

    let gate = Arc::new(Mutex::new(()));
    let _held = gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = view.sync_owned_live_tail(
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
    let _ = render(&mut view, &extended, 32, 4);
    assert!(
        view.visible
            .iter()
            .any(|row| row.key == EntryKey::cell(&extended[1]))
    );
}
