//! Layout readiness admits paint; terminal completion admits its routing geometry.

use super::*;
use crate::custom_terminal::output;
use crate::custom_terminal::output::FrameId;

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
            suppressed_header: self.suppressed_prompt_header.clone(),
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
            self.suppressed_prompt_header = frame.suppressed_header.clone();
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
        mode: input::MouseMode,
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

    pub(crate) fn scroll(&mut self, cells: &[Arc<dyn HistoryCell>], rows: isize) {
        if self.cache.frames.is_none() {
            self.scroll_prepared(cells, rows);
            return;
        }
        if self.area.is_empty() {
            return;
        }
        let previous = self.position;
        let scroll = match self.deferred_scroll.take() {
            Some(mut scroll) => {
                scroll.rows = scroll.rows.saturating_add(rows);
                scroll
            }
            None => {
                let Some(first) = self.visible.first() else {
                    return;
                };
                let offset = first.layout.position_at(first.row, 0);
                Scroll {
                    origin: Position::Reading(Anchor {
                        key: first.key,
                        index: first.index,
                        offset,
                        row_bias: first.layout.row_for_offset(offset) as isize - first.row as isize,
                    }),
                    rows,
                    cells: self
                        .snapshot_cells()
                        .or_else(|| self.presented_cells.clone())
                        .unwrap_or_else(|| Arc::from(cells)),
                }
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

    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer, cells: &[Arc<dyn HistoryCell>]) {
        if self.cache.frames.is_none() {
            self.render_prepared(area, buf, cells);
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
        let preparing_cells = inflight
            .as_ref()
            .map(|(_, cells)| cells.as_ref())
            .unwrap_or(cells);
        let mut prepared = Buffer::empty(area);
        self.render_prepared(area, &mut prepared, preparing_cells);
        if !self.cache.waiting && !self.live_preparation.is_pending() {
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
                suppressed_header: self.suppressed_prompt_header.clone(),
            });
        }
        if self.cache.waiting || self.live_preparation.is_pending() {
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
            self.suppressed_prompt_header = presented.suppressed_header.clone();
        } else {
            self.visible.clear();
            self.presented_cells = None;
            self.area = Rect::new(area.x, area.y, area.width, 0);
            self.tail_visible = false;
        }
    }

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
