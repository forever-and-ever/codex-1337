//! A conversation viewport over the app's retained cells and its current live tail.
//!
//! The app owns history and pagination. This view owns only reading/selection state and bounded
//! layout caches. Anchors address content inside an entry, so prepending history never renumbers
//! them and rewrapping does not turn a reading position into an unrelated screen row.
//! Recap tail spacing is reserved by layout and never enters selectable cell content.

use crate::history_cell::activity_parts::FoldState;

mod activity;
mod bookmark;
mod brackets;
mod composer_gap;
mod copy_mode;
pub(crate) mod dialogue;
mod disclosure;
mod disclosure_navigation;
mod feed;
mod follow_control;
mod footer;
mod input;
mod invocation;
mod layout;
mod live_preparation;
pub(crate) use live_preparation::OwnedLiveTail;
mod mutations;
mod pagination;
mod parked;
pub(crate) mod preparation;
mod presentation;
mod presented;
mod prompt_header;
mod search;
mod select_mode;
mod selection;
mod snapshot;
mod step_bills;
mod surface;
mod text;
mod tool_output_tokens;
mod turn_tip;
mod wheel;

use std::sync::Arc;

use crate::chatwidget::ActiveCellTranscriptKey;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::pager_overlay::TranscriptHistoryState;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::tui::SelectionCopy;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;

use layout::LayoutCache;
use search::Search;
use select_mode::SelectMode;
use selection::Selection;
use snapshot::ViewSnapshot;
pub(crate) use text::TextLayout;

pub(crate) use bookmark::TranscriptBookmark;
pub(crate) use feed::EntryKind;
pub(crate) use feed::FeedEntry;
pub(crate) use feed::HeaderDisclosure;
pub(crate) use input::JumpTarget;
pub(crate) use input::ViewAction;
pub(crate) use layout::ActivityTranscriptLines;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum EntryKey {
    Cell(usize),
    Live,
}

impl EntryKey {
    fn cell(cell: &Arc<dyn HistoryCell>) -> Self {
        Self::Cell(Arc::as_ptr(cell).cast::<()>() as usize)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Anchor {
    key: EntryKey,
    index: usize,
    offset: usize,
    // Synthetic rows share source offsets: positive biases precede source (separators),
    // while negative biases follow it (disclosure controls).
    row_bias: isize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Position {
    #[default]
    Latest,
    Reading(Anchor),
}

#[derive(Clone)]
struct VisibleRow {
    index: usize,
    row: usize,
    layout: Arc<TextLayout>,
    key: EntryKey,
    activity_ids: Arc<[String]>,
}

/// Shared scrolling and interaction state for compact and detailed transcript presentations.
pub(crate) struct TranscriptView {
    pub(crate) mouse_scroll_speed: f64,
    wheel: wheel::WheelScroll,
    pub(crate) copy_on_select: bool,
    pub(crate) copy_on_select_clear_selection: bool,
    pub(crate) primary_selection: bool,
    position: Position,
    follow_control: follow_control::FollowControl,
    pub(crate) show_follow_control: bool,
    copy_feedback: Option<composer_gap::CopyFeedback>,
    composer_tip: Option<(Rect, HyperlinkLine)>,
    turn_tip_key: Option<EntryKey>,
    cache: LayoutCache,
    presented: Option<presented::Presented>,
    presented_cells: Option<presented::Cells>,
    inflight_cells: Option<(u16, presented::Cells)>,
    deferred_scroll: Option<presented::Scroll>,
    deferred_focus: Option<crossterm::event::KeyEvent>,
    layout_source: presented::LayoutSource,
    deferred_folds:
        std::collections::VecDeque<(presented::Cells, usize, crate::keymap::ListAction)>,
    live_preparation: live_preparation::LivePreparation,
    live: Option<Arc<TextLayout>>,
    live_separated: Option<Arc<TextLayout>>,
    live_key: Option<(u16, ActiveCellTranscriptKey)>,
    live_continuation: bool,
    live_tool_entry: bool,
    area: Rect,
    prompt_header_target: Option<(Rect, EntryKey)>,
    suppressed_prompt_header: Option<prompt_header::SuppressedHeader>,
    visible: Vec<VisibleRow>,
    selection: Option<Selection>,
    select_mode: Option<SelectMode>,
    copy_mode: Option<copy_mode::CopyMode>,
    held_reading: Option<ViewSnapshot>,
    search: Search,
    detailed: bool,
    mode: HistoryRenderMode,
    pub(crate) history: TranscriptHistoryState,
    highlight: Option<usize>,
    saved_position: Option<Position>,
    unseen_activity: bool,
    tail_visible: bool,
    last_tail: Option<EntryKey>,
    last_click: Option<(std::time::Instant, u16, u16, u8)>,
    disclosure: disclosure::Disclosure,
    tool_output_tokens: tool_output_tokens::ToolOutputTokens,
    step_bills: step_bills::StepBills,
    invocations: invocation::Invocations,
    feed: feed::FeedIndex,
    dialogue: dialogue::Dialogue,
    lens: dialogue::Lens,
}

impl Default for TranscriptView {
    fn default() -> Self {
        Self {
            mouse_scroll_speed: 1.0,
            wheel: wheel::WheelScroll::default(),
            copy_on_select: false,
            copy_on_select_clear_selection: false,
            primary_selection: false,
            position: Position::Latest,
            follow_control: follow_control::FollowControl::default(),
            show_follow_control: true,
            copy_feedback: None,
            composer_tip: None,
            turn_tip_key: None,
            cache: LayoutCache::default(),
            presented: None,
            presented_cells: None,
            inflight_cells: None,
            deferred_scroll: None,
            deferred_focus: None,
            layout_source: presented::LayoutSource::Current,
            deferred_folds: Default::default(),
            live_preparation: Default::default(),
            live: None,
            live_separated: None,
            live_key: None,
            live_continuation: false,
            live_tool_entry: false,
            area: Rect::default(),
            prompt_header_target: None,
            suppressed_prompt_header: None,
            visible: Vec::new(),
            selection: None,
            select_mode: None,
            copy_mode: None,
            held_reading: None,
            search: Search::default(),
            detailed: false,
            mode: HistoryRenderMode::Rich,
            history: TranscriptHistoryState::Idle,
            highlight: None,
            saved_position: None,
            unseen_activity: false,
            tail_visible: true,
            last_tail: None,
            last_click: None,
            disclosure: disclosure::Disclosure::default(),
            tool_output_tokens: Default::default(),
            step_bills: Default::default(),
            invocations: Default::default(),
            feed: Default::default(),
            dialogue: Default::default(),
            lens: Default::default(),
        }
    }
}

impl TranscriptView {
    /// Rows left after the last render, including all startup notices and live entries.
    /// Callers can paint temporary UI here without changing selection or saved history.
    pub(crate) fn remaining_area(&self) -> Rect {
        let used = u16::try_from(self.visible.len())
            .unwrap_or(u16::MAX)
            .min(self.area.height);
        Rect {
            y: self.area.y + used,
            height: self.area.height - used,
            ..self.area
        }
    }

    fn render_prepared(&mut self, area: Rect, buf: &mut Buffer, cells: &[Arc<dyn HistoryCell>]) {
        self.composer_tip = None;
        self.prompt_header_target = None;
        self.cache.begin_frame();
        self.sync_history_tail(cells);
        let current_cells = cells;
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        let suppressed_prompt_header = self.suppressed_prompt_header.take();
        Clear.render(area, buf);
        let previous_height = self.area.height;
        let resized = self.area.width != area.width || self.area.bottom() != area.bottom();
        self.prepare_width(area.width);
        self.area = area;
        self.retain_synchronous_cells(cells);
        self.normalize_selection(cells);
        if resized {
            self.reveal_copy_context(cells);
        }
        self.visible.clear();
        if area.is_empty() {
            self.tail_visible = false;
            self.capture_synchronous_frame();
            return;
        }
        // Closing a disclosure can leave its reading anchor below the new scroll limit.
        let clamp_after_collapse = self.disclosure.clamp_after_collapse
            && self.selection.is_none()
            && matches!(self.position, Position::Reading(_));
        let render_start = |view: &mut Self| {
            let start = view.start(cells);
            if clamp_after_collapse {
                start.min(view.bottom_start(cells))
            } else {
                start
            }
        };
        let initial_start = render_start(self);
        let initial_position = self.position;
        let mut start = initial_start;
        let mut body = area;
        let header_position = prompt_header::SuppressedHeader {
            viewport: area,
            key: self.entry_key(cells, start.0),
            row: start.1,
        };
        if matches!(self.position, Position::Reading(_))
            && suppressed_prompt_header.as_ref() == Some(&header_position)
        {
            // Freezing follow must preserve a header that yielded to the next visible prompt.
            self.suppressed_prompt_header = Some(header_position);
        } else if area.height >= 4
            && prompt_header::line(cells, start.0, area.width, self.detailed).is_some()
        {
            body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
            let resized = resized || previous_height != body.height;
            self.area = body;
            // Copy targets must also leave context after the prompt reserves its row.
            if resized {
                self.reveal_copy_context(cells);
            }
            // The bottom limit changes after reserving the header row.
            start = render_start(self);
            if let Some((header, key)) =
                prompt_header::line(cells, start.0, area.width, self.detailed)
            {
                let header_area = Rect::new(area.x, area.y, area.width, /*height*/ 1);
                header.render(header_area, buf);
                self.prompt_header_target = Some((header_area, key));
            } else {
                body = area;
                self.area = area;
                start = initial_start;
                if resized && self.copy_mode.is_some() {
                    self.position = initial_position;
                }
                self.suppressed_prompt_header = Some(header_position);
            }
        }
        let area = body;
        let (mut index, mut row) = start;
        let mut entry_activity_ids: Option<(usize, Arc<[String]>)> = None;
        let mut entry_joins = None;
        for y in area.top()..area.bottom() {
            let Some(layout) = self.layout(cells, index) else {
                break;
            };
            if row >= layout.row_count() {
                index += 1;
                row = 0;
                // Empty entries are legal (for example hidden reasoning).
                let Some(next) = self.next_nonempty(cells, index) else {
                    break;
                };
                index = next;
            }
            let Some(layout) = self.layout(cells, index) else {
                break;
            };
            let key = self.entry_key(cells, index);
            let activity_ids = match &entry_activity_ids {
                Some((previous, ids)) if *previous == index => Arc::clone(ids),
                _ => {
                    let ids = self.displayed_activity_ids(cells, index);
                    entry_activity_ids = Some((index, Arc::clone(&ids)));
                    ids
                }
            };
            let row_area = Rect::new(area.x, y, area.width, /*height*/ 1);
            layout.render(row_area, buf, row);
            let joins = match entry_joins {
                Some((previous, joins)) if previous == index => joins,
                _ => {
                    let joins = self.joins(cells, index, &layout);
                    entry_joins = Some((index, joins));
                    joins
                }
            };
            if let Some(joins) = joins {
                layout.join_brackets(row_area, buf, row, joins);
            }
            self.render_disclosure(&activity_ids, &layout, row, row_area, buf);
            // A highlighted entry reads as selected whole: every row, edge to edge.
            if self.highlight == Some(index) && self.selection.is_none() && !self.search.is_active()
            {
                buf.set_style(row_area, crate::style::selection_style());
            }
            self.visible.push(VisibleRow {
                index,
                row,
                layout,
                key,
                activity_ids,
            });
            row += 1;
        }
        if self.selection_prepared()
            && let Some(pointer) = self
                .selection
                .as_ref()
                .filter(|selection| selection.dragging && selection.moved)
                .and_then(|selection| selection.pointer)
        {
            self.extend_selection(pointer.x, pointer.y);
        }
        self.tail_visible = self.current_tail_is_visible(current_cells);
        if self.tail_visible {
            self.unseen_activity = false;
        }
        self.render_selection(buf);
        self.render_select_cursor(buf);
        if self.history == TranscriptHistoryState::LoadingBeginning && self.is_following() {
            self.scroll(current_cells, /*rows*/ 0);
        }
        self.render_search(buf);
        self.capture_synchronous_frame();
    }

    /// Persist the clamped reading position after the last paint at the final viewport height.
    pub(crate) fn finish_collapse_layout(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        if self.area.is_empty()
            || self.cache.waiting
            || !std::mem::take(&mut self.disclosure.clamp_after_collapse)
            || self.selection.is_some()
            || !matches!(self.position, Position::Reading(_))
        {
            return;
        }
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        let start = self.start(cells);
        if let Some(first) = self.visible.first()
            && (first.index, first.row) < start
        {
            let (index, row, layout) = (first.index, first.row, Arc::clone(&first.layout));
            self.read_from_row(cells, index, row, &layout);
        }
    }

    pub(crate) fn sync_live_tail(
        &mut self,
        width: u16,
        key: Option<ActiveCellTranscriptKey>,
        lines: impl FnOnce(u16) -> Option<Vec<HyperlinkLine>>,
    ) -> bool {
        let mode = self.mode;
        self.sync_live_layout(width, key, |width, _| {
            lines(crate::transcript_geometry::content_width(width, mode)).map(|lines| {
                let lines = if mode == HistoryRenderMode::Rich {
                    crate::transcript_geometry::rich_lines(lines)
                } else {
                    lines
                };
                (
                    Arc::new(
                        TextLayout::new(lines, width)
                            .with_brackets(mode == HistoryRenderMode::Rich),
                    ),
                    key.is_some_and(|key| key.is_tool_entry),
                )
            })
        })
    }

    pub(crate) fn sync_live_activity_tail(
        &mut self,
        width: u16,
        key: Option<ActiveCellTranscriptKey>,
        expanded: bool,
        lines: impl FnOnce(
            u16,
            &FoldState,
            crate::history_cell::output_provenance::OutputMode,
        ) -> Option<ActivityTranscriptLines>,
    ) -> bool {
        let mode = self.mode;
        let output_mode = self.disclosure.output_mode(&self.disclosure.live_ids);
        self.sync_live_layout(width, key, |width, view| {
            lines(
                crate::transcript_geometry::content_width(width, mode),
                &view.disclosure.expanded.scoped(view.live_fold_scope()),
                output_mode,
            )
            .map(|mut lines| {
                let is_tool_entry = lines.is_tool_entry;
                let layout = if mode == HistoryRenderMode::Rich {
                    layout::activity_layout(
                        lines,
                        crate::transcript_geometry::GUTTER_COLUMNS,
                        width,
                        expanded,
                    )
                } else {
                    lines.activity.extend(lines.auxiliary);
                    TextLayout::new(lines.activity, width)
                };
                (
                    Arc::new(layout.with_brackets(mode == HistoryRenderMode::Rich)),
                    is_tool_entry,
                )
            })
        })
    }

    pub(crate) fn sync_live_layout(
        &mut self,
        width: u16,
        key: Option<ActiveCellTranscriptKey>,
        layout: impl FnOnce(u16, &Self) -> Option<(Arc<TextLayout>, bool)>,
    ) -> bool {
        let next = key.map(|key| (width, key));
        if key.is_some_and(|key| key.cacheable) && self.live_key == next {
            return false;
        }
        let revision_changed = self.live_key.is_none()
            || self.live_key.map(|(_, key)| key.revision) != next.map(|(_, key)| key.revision);
        self.live_key = next;

        self.live_continuation = key.is_some_and(|key| key.is_stream_continuation);
        let live = layout(width, self).map(|(layout, tool_entry)| {
            self.live_tool_entry = tool_entry;
            layout
        });
        if self
            .live
            .as_ref()
            .zip(live.as_ref())
            .is_some_and(|(old, new)| Arc::ptr_eq(old, new))
        {
            return false;
        }
        self.live_separated = None;
        if self.live.as_ref().map(|layout| layout.text())
            != live.as_ref().map(|layout| layout.text())
        {
            if !self.is_following() && revision_changed && live.is_some() {
                self.unseen_activity = true;
            }
            self.invalidate_live_search();
        }
        let changed = self.live.is_some() || live.is_some();
        self.live = live;
        changed
    }

    pub(crate) fn set_presentation(&mut self, detailed: bool, mode: HistoryRenderMode) {
        if self.detailed == detailed && self.mode == mode {
            return;
        }
        self.selection = None;
        self.select_mode = None;
        self.copy_mode = None;
        self.release_live_reading();
        self.cache.clear();
        self.suppressed_prompt_header = None;
        self.live_key = None;
        // Keep search's origin independent of either presentation's saved position.
        if self.detailed != detailed && !self.search.is_active() {
            let previous = self.position;
            self.position = self.saved_position.take().unwrap_or(previous);
            self.saved_position = Some(previous);
        }
        self.restart_search();
        self.detailed = detailed;
        self.mode = mode;
    }

    pub(crate) fn has_active_interaction(&self) -> bool {
        self.select_mode.is_some()
            || self.selection.is_some()
            || self.search.is_active()
            || self.is_activity_focused()
    }

    pub(crate) fn is_detailed(&self) -> bool {
        self.detailed
    }

    pub(crate) fn is_following(&self) -> bool {
        self.select_mode.is_none() && self.selection.is_none() && self.position == Position::Latest
    }

    /// Hold the current reading position until every older page has arrived.
    pub(crate) fn jump_to_beginning(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        if !self.detailed
            && !self.is_select_mode()
            && let Some(index) = cells
                .iter()
                .rposition(|cell| cell.as_any().is::<crate::history_cell::ClearScreenHeader>())
        {
            self.jump_to_entry(cells, index);
            return;
        }
        if self.history == TranscriptHistoryState::LoadingBeginning {
            return;
        }
        if self.history.has_unloaded_history() {
            self.scroll(cells, /*rows*/ 0);
            self.history = TranscriptHistoryState::LoadingBeginning;
        } else {
            self.jump_to_entry(cells, /*index*/ 0);
        }
    }

    pub(crate) fn jump_to_latest(&mut self) {
        self.cancel_deferred_navigation();
        self.disclosure.resume_after_approval = None;
        self.cancel_search();
        self.cancel_beginning();
        self.position = Position::Latest;
        self.selection = None;
        self.select_mode = None;
        self.copy_mode = None;
        self.release_live_reading();
        self.unseen_activity = false;
        self.disclosure.focused = None;
    }

    fn scroll_prepared(&mut self, cells: &[Arc<dyn HistoryCell>], rows: isize) {
        if rows != 0 {
            self.disclosure.resume_after_approval = None;
            self.last_click = None;
            self.cancel_beginning();
        }
        if self.area.is_empty() {
            return;
        }
        let current_cells = cells;
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        let start = self.start(cells);
        let (index, row) = self.move_rows(cells, start.0, start.1, rows);
        if rows < 0
            && (index, row) == start
            && self.is_following()
            && !self.history.has_unloaded_history()
        {
            return;
        }
        if (index, row) != start
            && let Some(selection) = &mut self.selection
        {
            selection.resume_on_empty = false;
        }
        // Anchor visible content, not a hidden header that stays ahead of every older page.
        let index = self.next_nonempty(cells, index).unwrap_or(index);
        if rows > 0 && (index, row) >= self.bottom_start(cells) {
            if self.selection.is_none() && !self.search.is_active() {
                self.jump_to_latest();
            } else {
                self.release_live_reading();
                self.position = Position::Latest;
            }
            return;
        }
        if let Some(layout) = self.layout(cells, index) {
            self.read_from_row(cells, index, row, &layout);
            self.hold_live_reading(current_cells, layout);
        }
    }

    fn read_from_row(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        row: usize,
        layout: &TextLayout,
    ) {
        let offset = layout.position_at(row, /*column*/ 0);
        self.position = Position::Reading(Anchor {
            key: self.entry_key(cells, index),
            index,
            offset,
            row_bias: layout.row_for_offset(offset) as isize - row as isize,
        });
    }

    pub(crate) fn jump_to_entry(&mut self, cells: &[Arc<dyn HistoryCell>], index: usize) {
        self.cancel_beginning();
        self.release_live_reading();
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        self.position = Position::Reading(Anchor {
            key: self.entry_key(cells, index),
            index,
            offset: self
                .layout(cells, index)
                .map_or(0, |layout| layout.content_start()),
            row_bias: 0,
        });
    }

    pub(crate) fn cancel_beginning(&mut self) {
        if self.history == TranscriptHistoryState::LoadingBeginning {
            // Keep the in-flight page, but let the user's new navigation supersede the jump.
            self.history = TranscriptHistoryState::LoadingOlder;
        }
    }

    pub(crate) fn ensure_entry_visible(&mut self, cells: &[Arc<dyn HistoryCell>], index: usize) {
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        if !self.visible.iter().any(|row| {
            let start = row
                .layout
                .position_at(row.row, /*column*/ 0)
                .max(row.layout.content_start());
            let end = row.layout.position_at(row.row, self.area.width);
            row.index == index && start < end && !row.layout.text()[start..end].trim().is_empty()
        }) {
            // Restoring a highlight after pagination must not cancel an explicit Home jump.
            let history = self.history;
            self.jump_to_entry(cells, index);
            self.history = history;
        }
    }

    pub(crate) fn set_highlight(&mut self, index: Option<usize>) {
        self.highlight = index;
    }

    pub(crate) fn needs_history(&mut self, cells: &[Arc<dyn HistoryCell>]) -> bool {
        self.search.needs_history(self.history)
            || (self.search.allows_viewport_paging() && self.near_start(cells))
    }

    pub(crate) fn near_start(&mut self, cells: &[Arc<dyn HistoryCell>]) -> bool {
        self.near_prepared_start(cells)
    }

    fn start(&mut self, cells: &[Arc<dyn HistoryCell>]) -> (usize, usize) {
        match self.position {
            Position::Latest => self.bottom_start(cells),
            Position::Reading(anchor) => {
                let index = self.resolve(cells, anchor);
                let row = self.layout(cells, index).map_or(/*default*/ 0, |layout| {
                    layout
                        .row_for_offset(anchor.offset)
                        .saturating_add_signed(anchor.row_bias.saturating_neg())
                        .min(layout.row_count().saturating_sub(/*rhs*/ 1))
                });
                self.position = Position::Reading(Anchor {
                    key: self.entry_key(cells, index),
                    index,
                    ..anchor
                });
                (index, row)
            }
        }
    }

    fn bottom_start(&mut self, cells: &[Arc<dyn HistoryCell>]) -> (usize, usize) {
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        let has_live = self.snapshot().map_or(self.live.is_some(), |snapshot| {
            snapshot.pinned.contains_key(&EntryKey::Live)
        });
        let count = cells.len() + usize::from(has_live);
        let Some(last) = count.checked_sub(/*rhs*/ 1) else {
            return (0, 0);
        };
        let height = self
            .layout(cells, last)
            .map_or(/*default*/ 0, |l| l.row_count());
        let recap_gap = usize::from(
            self.area.height > 1
                && !has_live
                && cells.last().is_some_and(|cell| {
                    cell.as_any()
                        .is::<crate::history_cell::ThreadRecapHistoryCell>()
                }),
        );
        self.move_rows(
            cells,
            last,
            height + recap_gap,
            -(self.area.height as isize),
        )
    }

    fn move_rows(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        mut index: usize,
        mut row: usize,
        rows: isize,
    ) -> (usize, usize) {
        if rows < 0 {
            let mut remaining = rows.unsigned_abs();
            while remaining > row && index > 0 {
                if !self.detailed
                    && cells.get(index).is_some_and(|cell| {
                        cell.as_any().is::<crate::history_cell::ClearScreenHeader>()
                    })
                {
                    return (index, 0);
                }
                remaining -= row;
                index -= 1;
                let Some(layout) = self.layout(cells, index) else {
                    return (index, 0);
                };
                row = layout.row_count();
            }
            // At the beginning, hidden entries are not preceding content. Keep the first
            // visible entry at the same position whether it is live or committed.
            if index == 0
                && remaining >= row
                && let Some(first) = self.next_nonempty(cells, index)
                && let Some(layout) = self.layout(cells, first)
            {
                return (first, usize::from(layout.separated));
            }
            return (index, row.saturating_sub(remaining));
        }
        let mut remaining = row.saturating_add(rows as usize);
        while let Some(layout) = self.layout(cells, index) {
            if remaining < layout.row_count() {
                break;
            }
            remaining -= layout.row_count();
            index += 1;
        }
        (index, remaining)
    }

    fn next_nonempty(&mut self, cells: &[Arc<dyn HistoryCell>], mut index: usize) -> Option<usize> {
        while let Some(layout) = self.layout(cells, index) {
            if layout.row_count() > 0 {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    fn resolve(&self, cells: &[Arc<dyn HistoryCell>], anchor: Anchor) -> usize {
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        if anchor.key == EntryKey::Live {
            return cells.len();
        }
        if cells
            .get(anchor.index)
            .is_some_and(|cell| EntryKey::cell(cell) == anchor.key)
        {
            return anchor.index;
        }
        cells
            .iter()
            .position(|cell| EntryKey::cell(cell) == anchor.key)
            .unwrap_or_else(|| anchor.index.min(cells.len().saturating_sub(/*rhs*/ 1)))
    }

    fn entry_key(&self, cells: &[Arc<dyn HistoryCell>], index: usize) -> EntryKey {
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        cells.get(index).map_or(EntryKey::Live, EntryKey::cell)
    }
}

#[cfg(test)]
#[path = "transcript_view_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "transcript_view/markdown_copy_tests.rs"]
mod markdown_copy_tests;

#[cfg(test)]
#[path = "transcript_view/copy_on_select_tests.rs"]
mod copy_on_select_tests;

#[cfg(test)]
#[path = "transcript_view/command_parts_tests.rs"]
mod command_parts_tests;

#[cfg(test)]
#[path = "transcript_view/tool_entries_tests.rs"]
mod tool_entries_tests;
