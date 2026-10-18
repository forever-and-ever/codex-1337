//! Prepared caches retain small immutable entries beyond the legacy count limit.

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn small_entries_are_retained_beyond_the_old_count_limit() {
    let cells: Vec<_> = (0..100).map(|_| Arc::new(Cell::default())).collect();
    let mut cache = LayoutCache::default();
    cache.begin_frame();
    for cell in &cells {
        let history = cell.clone() as Arc<dyn HistoryCell>;
        let _ = cache.get(
            &history,
            /*width*/ 20,
            CellPresentation {
                separated: false,
                turn_tip_space: false,
                expanded: false,
                disclosure: false,
                continued_edit: false,
                feed: 0,
                detailed: false,
            },
            || {
                let history = Arc::clone(&history);
                Box::new(move || {
                    TextLayout::new(
                        history.transcript_hyperlink_lines(/*width*/ 20),
                        /*width*/ 20,
                    )
                })
            },
        );
    }
    for index in [99, 98, 0] {
        let history = cells[index].clone() as Arc<dyn HistoryCell>;
        let _ = cache.get(
            &history,
            /*width*/ 20,
            CellPresentation {
                separated: false,
                turn_tip_space: false,
                expanded: false,
                disclosure: false,
                continued_edit: false,
                feed: 0,
                detailed: false,
            },
            || {
                let history = Arc::clone(&history);
                Box::new(move || {
                    TextLayout::new(
                        history.transcript_hyperlink_lines(/*width*/ 20),
                        /*width*/ 20,
                    )
                })
            },
        );
    }
    assert_eq!(
        [99, 98, 0].map(|index| cells[index].renders.load(Ordering::Relaxed)),
        [1, 1, 1]
    );
}
