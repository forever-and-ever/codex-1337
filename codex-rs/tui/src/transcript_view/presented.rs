//! Layout readiness admits paint; terminal completion admits its routing geometry.

use super::Arc;
use super::Buffer;
use super::Clear;
use super::EntryKey;
use super::HistoryCell;
use super::HyperlinkLine;
use super::Position;
use super::Rect;
use super::TranscriptView;
use super::ViewAction;
use super::VisibleRow;
use super::Widget;
use super::input;
use super::prompt_header;
use crate::custom_terminal::output;
use crate::custom_terminal::output::FrameId;

pub(super) type Cells = Arc<[Arc<dyn HistoryCell>]>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutSource {
    Current,
    Presented,
}

pub(super) struct Scroll {
    origin: Position,
    rows: isize,
    cells: Arc<[Arc<dyn HistoryCell>]>,
}

pub(super) struct Presented {
    buffer: Buffer,
    cells: Arc<[Arc<dyn HistoryCell>]>,
    frames: FrameRecords,
    area: Rect,
    visible: Vec<VisibleRow>,
    tail_visible: bool,
    suppressed_header: Option<prompt_header::SuppressedHeader>,
}

#[derive(Default)]
struct FrameRecords {
    completed: Option<(FrameId, RoutingFrame)>,
    pending: Option<(FrameId, RoutingFrame)>,
}

struct RoutingFrame {
    area: Rect,
    visible: Vec<VisibleRow>,
    tail_visible: bool,
    suppressed_header: Option<prompt_header::SuppressedHeader>,
    cells: Option<Arc<[Arc<dyn HistoryCell>]>>,
    follow_area: Option<Rect>,
    composer_tip: Option<(Rect, HyperlinkLine)>,
}

impl FrameRecords {
    fn promote(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(id, _)| Some(*id) == output::presented_frame())
        {
            self.completed = self.pending.take();
        }
    }
}

impl TranscriptView {
    pub(crate) fn capture_painted_frame(&mut self) {
        let Some(id) = output::current_frame() else {
            return;
        };
        let frame = RoutingFrame {
            area: self.area,
            visible: self.visible.clone(),
            tail_visible: self.tail_visible,
            suppressed_header: self.suppressed_prompt_header,
            cells: self.presented_cells.clone(),
            follow_area: self.follow_control.area,
            composer_tip: self.composer_tip.clone(),
        };
        let presented = self.presented.get_or_insert_with(|| Presented {
            buffer: Buffer::empty(Rect::ZERO),
            cells: Arc::from([]),
            frames: FrameRecords::default(),
            area: Rect::ZERO,
            visible: Vec::new(),
            tail_visible: false,
            suppressed_header: None,
        });
        presented.frames.promote();
        presented.frames.pending = Some((id, frame));
    }

    pub(crate) fn promote_presented_frame(&mut self) {
        if !output::is_active() {
            return;
        }
        if let Some(presented) = &mut self.presented {
            presented.frames.promote();
        }
        let completed = self
            .presented
            .as_ref()
            .and_then(|presented| presented.frames.completed.as_ref());
        if let Some((_, frame)) = completed {
            self.area = frame.area;
            self.visible = frame.visible.clone();
            self.tail_visible = frame.tail_visible;
            self.suppressed_prompt_header = frame.suppressed_header;
            self.presented_cells = frame.cells.clone();
            self.follow_control.area = frame.follow_area;
            self.composer_tip = frame.composer_tip.clone();
        } else {
            self.area.height = 0;
            self.visible.clear();
            self.tail_visible = false;
            self.presented_cells = None;
            self.follow_control.area = None;
            self.composer_tip = None;
        }
    }

    pub(super) fn capture_synchronous_frame(&mut self) {
        if self.cache.frames.is_none() {
            self.capture_painted_frame();
        }
    }
}

impl TranscriptView {
    pub(crate) fn navigate_pager(
        &mut self,
        key: crossterm::event::KeyEvent,
        cells: &[Arc<dyn HistoryCell>],
        keymap: &crate::keymap::PagerKeymap,
    ) -> bool {
        if self.cache.frames.is_none() {
            return self.navigate_presented_pager(key, cells, keymap);
        }
        let presented = self
            .snapshot_cells()
            .or_else(|| self.presented_cells.clone());
        let cells = presented.as_deref().unwrap_or(&[]);
        self.layout_source = LayoutSource::Presented;
        let result = self.navigate_presented_pager(key, cells, keymap);
        self.layout_source = LayoutSource::Current;
        result
    }

    pub(crate) fn handle_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        cells: &[Arc<dyn HistoryCell>],
    ) -> Option<ViewAction> {
        if self.cache.frames.is_none() {
            return self.handle_presented_key(key, cells);
        }
        let presented = self
            .snapshot_cells()
            .or_else(|| self.presented_cells.clone());
        let cells = presented.as_deref().unwrap_or(&[]);
        self.deferred_focus = None;
        let waiting = std::mem::replace(&mut self.cache.waiting, false);
        self.layout_source = LayoutSource::Presented;
        let result = self.handle_presented_key(key, cells);
        self.layout_source = LayoutSource::Current;
        self.cache.waiting |= waiting;
        result
    }

    pub(super) fn handle_mouse_with_mode(
        &mut self,
        event: crossterm::event::MouseEvent,
        cells: &[Arc<dyn HistoryCell>],
        mode: input::MouseMode<'_>,
    ) -> Option<ViewAction> {
        if self.cache.frames.is_none() {
            return self.handle_presented_mouse(event, cells, mode);
        }
        let presented = self
            .snapshot_cells()
            .or_else(|| self.presented_cells.clone());
        let cells = presented.as_deref().unwrap_or(&[]);
        self.layout_source = LayoutSource::Presented;
        let result = self.handle_presented_mouse(event, cells, mode);
        self.layout_source = LayoutSource::Current;
        result
    }

    pub(super) fn cancel_deferred_navigation(&mut self) {
        self.deferred_scroll = None;
        self.deferred_focus = None;
        self.deferred_folds.clear();
        self.inflight_cells = None;
    }

    pub(crate) fn scroll(&mut self, cells: &[Arc<dyn HistoryCell>], rows: isize) {
        if self.cache.frames.is_none() {
            self.scroll_prepared(cells, rows);
            return;
        }
        if self.area.is_empty() {
            return;
        }
        let previous = self.position;
        // Reversal supersedes movement blocked on preparation; it must not first pay off old input.
        let pending = self
            .deferred_scroll
            .take()
            .filter(|scroll| rows == 0 || scroll.rows.signum() == rows.signum());
        let scroll = if let Some(mut scroll) = pending {
            scroll.rows = scroll.rows.saturating_add(rows);
            scroll
        } else {
            Scroll {
                // Relative input composes against intent, not stale pointer-routing geometry.
                origin: previous,
                rows,
                cells: self
                    .snapshot_cells()
                    .or_else(|| self.presented_cells.clone())
                    .unwrap_or_else(|| Arc::from(cells)),
            }
        };
        self.position = scroll.origin;
        let waiting = std::mem::replace(&mut self.cache.waiting, false);
        self.layout_source = LayoutSource::Presented;
        self.scroll_prepared(&scroll.cells, scroll.rows);
        self.layout_source = LayoutSource::Current;
        if self.cache.waiting {
            self.position = previous;
            self.deferred_scroll = Some(scroll);
        }
        self.cache.waiting |= waiting;
    }

    #[expect(
        clippy::expect_used,
        reason = "A ready frame installs its retained cells before publishing the presented revision."
    )]
    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer, cells: &[Arc<dyn HistoryCell>]) {
        if self.cache.frames.is_none() {
            self.render_prepared(area, buf, cells);
            self.finish_collapse_layout(cells);
            self.paint_tool_marks(buf);
            return;
        }
        if self.deferred_scroll.is_some() && self.area.width == area.width {
            self.scroll(cells, 0);
        }
        if let Some(key) = self.deferred_focus.take() {
            self.handle_key(key, cells);
        }
        self.resume_fold_navigation();
        let inflight = self
            .inflight_cells
            .take()
            .filter(|(width, _)| *width == area.width);
        let preparing_cells = inflight.as_ref().map_or(cells, |(_, cells)| cells.as_ref());
        let mut prepared = Buffer::empty(area);
        self.render_prepared(area, &mut prepared, preparing_cells);
        // The live tail is already a complete immutable revision. Its successor
        // must not hold scrolling or newly committed cells behind preparation.
        if !self.cache.waiting {
            self.prefetch(preparing_cells);
            self.cache.waiting = false;
            if !self.presented_cells.as_ref().is_some_and(|old| {
                old.len() == preparing_cells.len()
                    && old
                        .iter()
                        .zip(preparing_cells)
                        .all(|(a, b)| Arc::ptr_eq(a, b))
            }) {
                self.presented_cells = Some(Arc::from(preparing_cells));
            }
            if inflight.is_some()
                && let Some(frames) = &self.cache.frames
            {
                frames.schedule_frame();
            }
            let frames = self
                .presented
                .take()
                .map(|presented| presented.frames)
                .unwrap_or_default();
            self.presented = Some(Presented {
                buffer: prepared,
                cells: self
                    .presented_cells
                    .clone()
                    .expect("prepared cells retained"),
                frames,
                area: self.area,
                visible: self.visible.clone(),
                tail_visible: self.tail_visible,
                suppressed_header: self.suppressed_prompt_header,
            });
        }
        if self.cache.waiting {
            self.inflight_cells = Some(inflight.unwrap_or_else(|| (area.width, Arc::from(cells))));
        }
        Clear.render(area, buf);
        if let Some(presented) = &self.presented
            && presented.buffer.area.width == area.width
        {
            let overlap = area.intersection(presented.buffer.area);
            for y in overlap.top()..overlap.bottom() {
                for x in overlap.left()..overlap.right() {
                    buf[(x, y)] = presented.buffer[(x, y)].clone();
                }
            }
            self.area = presented.area.intersection(area);
            self.visible = presented
                .visible
                .iter()
                .skip(usize::from(self.area.y.saturating_sub(presented.area.y)))
                .take(usize::from(self.area.height))
                .cloned()
                .collect();
            self.presented_cells = Some(Arc::clone(&presented.cells));
            self.tail_visible = presented.tail_visible;
            self.suppressed_prompt_header = presented.suppressed_header;
        } else {
            self.visible.clear();
            self.presented_cells = None;
            self.area = Rect::new(area.x, area.y, area.width, 0);
            self.tail_visible = false;
        }
        self.paint_tool_marks(buf);
    }

    #[expect(
        clippy::expect_used,
        reason = "A visible first row guarantees that the same immutable slice has a last row."
    )]
    fn prefetch(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        let Some(first) = self.visible.first().map(|row| row.index) else {
            return;
        };
        let last = self.visible.last().expect("visible has a first row").index;
        for index in first.saturating_sub(2)..first {
            self.current_layout(cells, index);
        }
        for index in last + 1..=(last + 2).min(cells.len().saturating_sub(1)) {
            self.current_layout(cells, index);
        }
    }
}

impl TranscriptView {
    pub(crate) fn presented_cells(&self) -> Option<Arc<[Arc<dyn HistoryCell>]>> {
        self.presented_cells.clone()
    }

    pub(super) fn rewrap_presented_snapshot(&mut self, width: u16) {
        let Some(snapshot) = self.snapshot() else {
            return;
        };
        let pinned: Vec<_> = snapshot
            .pinned
            .iter()
            .map(|(key, layout)| (*key, Arc::clone(layout)))
            .collect();
        let live = snapshot.search_live.clone();
        for (key, layout) in pinned {
            if let Some(layout) = self.cache.rewrap(&layout, width, false)
                && let Some(snapshot) = self.snapshot_mut()
            {
                snapshot.pinned.insert(key, layout);
            }
        }
        if let Some(live) = live
            && let Some(layout) = self.cache.rewrap(&live, width, false)
            && let Some(snapshot) = self.snapshot_mut()
        {
            snapshot.search_live = Some(layout);
        }
    }
}

impl TranscriptView {
    pub(super) fn retain_synchronous_cells(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        // Synchronous rendering presents at once; background presentation commits in presented.rs.
        if self.cache.frames.is_none()
            && self.presented_cells.as_ref().is_none_or(|presented| {
                presented.len() != cells.len()
                    || presented
                        .iter()
                        .zip(cells)
                        .any(|(previous, current)| !Arc::ptr_eq(previous, current))
            })
        {
            self.presented_cells = Some(Arc::from(cells));
        }
    }

    pub(super) fn selection_prepared(&self) -> bool {
        !self.cache.waiting
    }
}

impl TranscriptView {
    pub(super) fn presented_activity_ids(
        &self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
    ) -> Vec<String> {
        if self.layout_source == LayoutSource::Presented {
            if self.presented_cells.is_none() {
                return Vec::new();
            }
            let key = cells.get(index).map_or(EntryKey::Live, EntryKey::cell);
            if let Some(visible) = self.visible.iter().find(|visible| visible.key == key) {
                return visible.activity_ids.to_vec();
            }
        }
        let Some(cell) = cells.get(index) else {
            return self.disclosure.live_ids.clone();
        };
        let ids = cell.activity_ids();
        // A Feed Item's header over a cell without identities is focused through its fold.
        if ids.is_empty()
            && self.indexed_seat(cells, index).header.is_some()
            && let Some(fold) = self.feed.fold_of(index)
        {
            return vec![fold.to_string()];
        }
        ids
    }
}

#[cfg(test)]
#[path = "preparation_progress_tests.rs"]
mod tests;
