//! Width-specific layouts retained only for recently displayed conversation entries.
//!
//! Mutable cells refresh each frame, then share that frame's layout across measurement and paint.
//! Stable cells also invalidate when animation ticks, syntax themes, or terminal colors change.
//! A Feed Item's header is a row of the entry it heads, so committing live work never moves
//! the entries around it; see `feed`.

use super::feed;
use super::feed::Content;
use super::feed::Seat;
use crate::history_cell::ActivityDisclosure;
use crate::history_cell::ReasoningSummaryCell;
use crate::history_cell::ToolOutputGroup;
use crate::history_cell::output_provenance::OutputMode;
use crate::transcript_geometry::TokenSeat;
use std::collections::HashSet;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Weak;

use super::*;

#[path = "layout_rewrap.rs"]
mod rewrap;

const MAX_CACHED_TEXT_BYTES: usize = 32 * 1024 * 1024;
/// The columns a clipped first row keeps before tokens that trail it give way.
const LEGIBLE_ROW: usize = 16;

/// The selected activity presentation and any live content that follows it.
pub(crate) struct ActivityTranscriptLines {
    pub(crate) output_mode: Option<OutputMode>,
    pub(crate) is_tool_entry: bool,
    pub(crate) activity: Vec<HyperlinkLine>,
    pub(crate) header: Option<crate::history_cell::activity_parts::ActivityHeader>,
    pub(crate) auxiliary: Vec<HyperlinkLine>,
    /// The row's tokens in the gutter, when no Feed Item bills it.
    pub(crate) header_tokens: Option<u64>,
    pub(crate) disclosure: Option<ActivityDisclosure>,
    pub(crate) controls: Vec<crate::history_cell::activity_parts::PartControl>,
}

/// Keep disclosure controls outside selectable source without rendering hidden details. Rows
/// start `columns` from the edge; the activity's tokens trail its first.
pub(super) fn activity_layout(
    mut lines: ActivityTranscriptLines,
    columns: u16,
    width: u16,
    expanded: bool,
) -> TextLayout {
    // A truncated header can create the control even when there is no output. Once open,
    // its untruncated invocation must still have a control to close it.
    if expanded && !lines.activity.is_empty() {
        lines.disclosure.get_or_insert(ActivityDisclosure::Generic);
    }
    // The activity's own control takes the first row its parts leave free.
    let mut control_line = match lines.disclosure {
        Some(ActivityDisclosure::EditedFile { header }) => usize::from(header),
        _ => 0,
    };
    while control_line + 1 < lines.activity.len()
        && lines.controls.iter().any(|part| part.line == control_line)
    {
        control_line += 1;
    }
    if lines.is_tool_entry
        && (lines.header.is_some() || lines.disclosure.is_some() || !lines.controls.is_empty())
        && !expanded
        && !lines.controls.iter().any(|control| control.expanded)
    {
        // Tokens that trail the first row keep their room on it while the row stays legible.
        let available = usize::from(crate::transcript_geometry::content_width(
            width,
            HistoryRenderMode::Rich,
        ));
        let trailing = lines
            .header_tokens
            .map_or(0, |tokens| 2 + super::text::token_label(tokens).len());
        let trailing = if available >= LEGIBLE_ROW + trailing {
            trailing
        } else {
            0
        };
        for (row, header) in lines.activity.iter_mut().enumerate() {
            let available = available - if row == 0 { trailing } else { 0 };
            if header.width() > available {
                if lines
                    .header
                    .is_none_or(|header| header.expand_on_truncation)
                {
                    lines.disclosure.get_or_insert(ActivityDisclosure::Generic);
                }
                let prefix_bytes = header
                    .source
                    .as_ref()
                    .map_or(0, |source| source.prefix_bytes);
                let line = crate::history_cell::activity_preview::compact_header(
                    header.line.clone(),
                    available,
                );
                let source = crate::terminal_hyperlinks::LogicalLineSource::from_line(&line);
                let body =
                    source.styled_range(prefix_bytes.min(source.range.end)..source.range.end);
                let mut source = crate::terminal_hyperlinks::LogicalLineSource::from_line(&body);
                source.prefix_bytes = prefix_bytes.min(line.to_string().len());
                let clock = header.clock;
                *header = line.into();
                header.clock = clock;
                header.source = Some(source);
            }
        }
    }
    let has_details = !lines.activity.is_empty() && (expanded || lines.disclosure.is_some());
    let mut rows = lines.activity;
    rows.extend(lines.auxiliary);
    let mut layout = TextLayout::new(
        crate::transcript_geometry::gutter_lines(rows, columns),
        width,
    )
    .with_header_tokens(lines.header_tokens, TokenSeat::Trailing);
    if has_details {
        layout = layout.with_leading_disclosure_control(
            crate::render::disclosure::glyph(expanded).into(),
            control_line,
        );
    }
    for part in lines.controls {
        layout = layout.with_part_control(part);
    }
    if let (Some(mode), Some(header)) = (lines.output_mode, lines.header) {
        let offset = layout.line_offset(0) + header.source_offset;
        layout = layout.with_output_controls(offset, mode);
    }
    layout
}

/// Give an activity's first row to its header, keeping the rows after it and their controls.
/// Only a nonempty source row holds a control; blank or indentation-only rows do not.
fn holds_control(line: &HyperlinkLine) -> bool {
    line.width() > 0
        && line
            .source
            .as_ref()
            .is_none_or(|source| !source.text.is_empty())
}

fn yield_head(lines: &mut ActivityTranscriptLines) {
    let dropped = feed::behead(&mut lines.activity);
    lines.header = None;
    lines
        .controls
        .retain_mut(|part| match part.line.checked_sub(dropped) {
            Some(line) => {
                part.line = line;
                true
            }
            None => false,
        });
    if dropped > 0 && lines.disclosure == Some(ActivityDisclosure::EditedFile { header: true }) {
        lines.disclosure = Some(ActivityDisclosure::EditedFile { header: false });
    }
}

#[derive(Default)]
pub(super) struct LayoutCache {
    entries: std::collections::HashMap<CacheKey, CachedLayout>,
    frame_keys: std::collections::HashMap<RevisionKey, CacheKey>,
    recency: std::collections::BTreeMap<u64, CacheKey>,
    access: u64,
    bytes: usize,
    frame: u64,
    render_state: Option<RenderState>,
    pub(super) frames: Option<crate::tui::FrameRequester>,
    pending: Vec<PendingLayout>,
    pub(super) waiting: bool,
    rewraps: Vec<rewrap::Rewrap>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct RenderState {
    theme: u64,
    foreground: Option<(u8, u8, u8)>,
    background: Option<(u8, u8, u8)>,
    color_level: crate::terminal_palette::StdoutColorLevel,
}

impl RenderState {
    pub(super) fn current() -> Self {
        Self {
            theme: crate::render::highlight::syntax_theme_revision(),
            foreground: crate::terminal_palette::default_fg(),
            background: crate::terminal_palette::default_bg(),
            color_level: crate::terminal_palette::stdout_color_level(),
        }
    }
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct RevisionKey {
    identity: usize,
    width: u16,
    presentation: CellPresentation,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct CacheKey {
    revision: RevisionKey,
    animation_tick: Option<u64>,
}

struct CachedLayout {
    source: Weak<dyn HistoryCell>,
    access: u64,
    layout: Arc<TextLayout>,
    rendered_frame: u64,
    refreshed_at: std::time::Instant,
}

struct PendingLayout {
    source: Weak<dyn HistoryCell>,
    requested_frame: u64,
    width: u16,
    presentation: CellPresentation,
    animation_tick: Option<u64>,
    task: super::preparation::Pending<Arc<TextLayout>>,
}

#[derive(Clone, Copy, Default, Hash, PartialEq, Eq)]
pub(super) struct CellPresentation {
    separated: bool,
    turn_tip_space: bool,
    expanded: bool,
    disclosure: bool,
    continued_edit: bool,
    /// The digest of the entry's feed rows; zero when it draws as a loose entry.
    feed: u64,
    detailed: bool,
}

impl CellPresentation {
    pub(super) fn decorate(self, mut layout: TextLayout) -> TextLayout {
        if self.separated {
            layout = layout.with_leading_separator();
        }
        if self.turn_tip_space {
            layout = layout.with_leading_spacer();
        }
        layout
    }
}

impl TranscriptView {
    /// Only visible, unfrozen retained activity requests another animation frame.
    pub(crate) fn has_visible_animation(&self, cells: &[Arc<dyn HistoryCell>]) -> bool {
        self.is_following()
            && self.snapshot().is_none()
            && self.visible.iter().any(|row| {
                cells.get(row.index).is_some_and(|cell| {
                    row.key == EntryKey::cell(cell) && cell.transcript_animation_tick().is_some()
                })
            })
    }

    pub(super) fn layout(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
    ) -> Option<Arc<TextLayout>> {
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        if index > cells.len() {
            return None;
        }
        let key = self.entry_key(cells, index);
        if let Some(layout) = self.search.match_layout(key) {
            return self.cache.rewrap(&layout, self.area.width, false);
        }
        self.base_layout(cells, index)
    }

    /// Resolve manual disclosure and retained revisions, without Find's temporary expansion.
    /// Call with the snapshot's cells, if present.
    pub(super) fn base_layout(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
    ) -> Option<Arc<TextLayout>> {
        let key = self.entry_key(cells, index);
        if let Some(layout) = self
            .snapshot()
            .and_then(|snapshot| snapshot.pinned.get(&key))
        {
            let layout = Arc::clone(layout);
            return self.cache.rewrap(&layout, self.area.width, false);
        }
        if index == cells.len() && self.snapshot().is_some() {
            return None;
        }
        self.current_layout(cells, index)
    }

    /// Resolve the current revision without a reader's retained snapshot.
    pub(super) fn current_layout(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
    ) -> Option<Arc<TextLayout>> {
        if self.layout_source == super::presented::LayoutSource::Presented {
            if self.presented_cells.is_none() {
                return None;
            }
            let key = self.entry_key(cells, index);
            if let Some(visible) = self.visible.iter().find(|visible| visible.key == key) {
                return Some(Arc::clone(&visible.layout));
            }
        }
        self.entry_layout(cells, index, self.detailed)
    }

    pub(super) fn entry_layout(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        full_content: bool,
    ) -> Option<Arc<TextLayout>> {
        if index > cells.len() {
            return None;
        }
        // Search reads retained entry content independently of its manual folds.
        let seat = if full_content
            && cells
                .get(index)
                .is_some_and(|cell| feed::feed_entry(cell.as_ref()).is_some())
        {
            super::feed::Seat::LOOSE
        } else {
            self.feed_seat(cells, index)
        };
        if self.elides_cell(cells, index) || !seat.is_visible() {
            return Some(Arc::new(TextLayout::new(
                Vec::new(),
                self.area.width.max(/*other*/ 1),
            )));
        }
        let previous_index = self.previous_visible(cells, index);
        let previous = previous_index.and_then(|previous| cells.get(previous));
        let width = self.area.width.max(/*other*/ 1);
        let Some(cell) = cells.get(index) else {
            let live = self.live.as_ref()?;
            if !crate::transcript_geometry::separated(
                previous.map(AsRef::as_ref),
                self.live_tool_entry,
                self.live_continuation,
            ) {
                return Some(Arc::clone(live));
            }
            return Some(Arc::clone(self.live_separated.get_or_insert_with(|| {
                Arc::new(live.rewrap(width).with_leading_separator())
            })));
        };
        // Session information belongs before all history, including pages not loaded yet.
        // Pinned displayed revisions above remain visible; hidden headers never enter the cache.
        if self.history.has_unloaded_history()
            && cell.as_any().is::<crate::history_cell::SessionInfoCell>()
        {
            return Some(Arc::new(TextLayout::new(Vec::new(), width)));
        }
        let mode = self.mode;
        let detailed = self.detailed;
        let loose = seat.content == Content::Loose;
        let ids = cell.activity_ids();
        let disclosure = !detailed && mode == HistoryRenderMode::Rich && !ids.is_empty();
        let manually_expanded = self.disclosure.is_expanded(&ids);
        let expanded = disclosure && (full_content || manually_expanded);
        let output_mode = self.disclosure.output_mode(&ids);
        // A member's figures belong to its item's header; a loose row keeps its own tokens.
        let header_tokens = loose
            .then(|| {
                cell.activity_header_tokens()
                    .or_else(|| self.activity_output_tokens(&ids))
            })
            .flatten();
        let continued_edit = disclosure
            && !cell.is_stream_continuation()
            && previous.is_some_and(|previous| {
                ToolOutputGroup::shares_patch_heading(cell.as_ref(), previous.as_ref())
            });
        if manually_expanded && crate::history_cell::invocation::collect(cell.as_ref()).is_empty() {
            self.disclosure.expanded.extend(ids.iter().cloned());
        }
        let separated = match &seat.header {
            Some(header) => {
                previous_index.is_some()
                    && (header.spacious
                        || !previous_index.is_some_and(|previous| self.feed.compact(previous)))
            }
            None if loose => crate::transcript_geometry::separated(
                previous.map(AsRef::as_ref),
                cell.is_tool_entry(),
                cell.is_stream_continuation(),
            ),
            None => false,
        };
        let presentation = CellPresentation {
            separated,
            turn_tip_space: self.turn_tip_key == Some(EntryKey::cell(cell)),
            expanded,
            disclosure,
            continued_edit,
            feed: feed_digest(&seat, self.feed.revision, self.step_bills.revision()),
            detailed: full_content,
        };
        let expanded_parts = &self.disclosure.expanded;
        let feed = &self.feed;
        let bills = &self.step_bills;
        let invocations = &self.invocations;
        let output_tokens = &self.tool_output_tokens;
        let input_open = self.feed.input_open(cells, index);
        let background = self.cache.frames.is_some();
        self.cache.get(cell, width, presentation, || {
            let cell = Arc::clone(cell);
            let header = seat
                .header
                .as_ref()
                .map(|header| feed.prepare_header(cells, header, bills));
            let snapshot = background.then(|| {
                invocations.snapshot(
                    std::iter::once(cell.as_ref())
                        .chain(header.iter().flat_map(feed::HeaderPreparation::sources)),
                )
            });
            let token_ids: Vec<_> = cell
                .activity_ids()
                .into_iter()
                .chain(
                    header
                        .iter()
                        .flat_map(feed::HeaderPreparation::sources)
                        .flat_map(HistoryCell::activity_ids),
                )
                .collect();
            let output_tokens = output_tokens.subset(&token_ids);
            let expanded_parts = expanded_parts.clone();
            let render = move |invocations: &super::invocation::Invocations| {
                let content_width = crate::transcript_geometry::content_width(width, mode);
                // A member's clock is its item's header's.
                let timestamp = if loose {
                    cell.transcript_timestamp()
                } else {
                    None
                };
                // The activity's rows, compact or expanded, and what controls them.
                let activity = |expanded: bool, expanded_parts: &HashSet<String>| {
                    let mut lines = if !full_content
                        && let Some(parts) = cell.output_parts(
                            content_width,
                            /*reserve*/ 0,
                            expanded_parts,
                            output_mode,
                        ) {
                        ActivityTranscriptLines {
                            output_mode: ((expanded
                                || cell.activity_disclosure(content_width).is_none())
                                && cell.has_formatted_output())
                            .then_some(output_mode),
                            is_tool_entry: cell.is_tool_entry(),

                            activity: crate::transcript_geometry::clock_lines(
                                parts.lines,
                                timestamp,
                            ),
                            header: parts.header,
                            auxiliary: Vec::new(),
                            header_tokens,
                            disclosure: parts.output,
                            controls: parts.controls,
                        }
                    } else {
                        let activity = if expanded {
                            cell.expanded_output_lines(content_width, output_mode)
                        } else {
                            cell.compact_hyperlink_lines(content_width)
                        };
                        ActivityTranscriptLines {
                            output_mode: ((expanded
                                || cell.activity_disclosure(content_width).is_none())
                                && cell.has_formatted_output())
                            .then_some(output_mode),
                            is_tool_entry: cell.is_tool_entry(),

                            activity: crate::transcript_geometry::clock_lines(activity, timestamp),
                            header: if expanded {
                                cell.expanded_activity_header()
                            } else {
                                cell.activity_header()
                            },
                            auxiliary: Vec::new(),
                            header_tokens,
                            disclosure: cell.activity_disclosure(content_width),
                            controls: Vec::new(),
                        }
                    };
                    if continued_edit {
                        yield_head(&mut lines);
                    }
                    lines
                };
                let invocation = (mode == HistoryRenderMode::Rich
                    && !detailed
                    && seat.content != Content::Hidden)
                    .then(|| {
                        invocations.layout(
                            cell.as_ref(),
                            super::invocation::PreparationSource::Retained(&cell),
                            width,
                            input_open,
                            seat.content,
                            full_content,
                            &output_tokens,
                            &expanded_parts,
                            Vec::new(),
                        )
                    })
                    .flatten();
                let mut layout = if let Some(layout) = invocation {
                    layout
                } else {
                    match seat.content {
                        Content::Hidden => TextLayout::new(Vec::new(), width),
                        Content::Body => {
                            let parts = feed::entry_body(
                                cell.as_ref(),
                                content_width,
                                &expanded_parts,
                                output_mode,
                            )
                            .expect("feed body requires a feed entry");
                            let mut layout = TextLayout::new(
                                crate::transcript_geometry::rich_lines(parts.lines),
                                width,
                            );
                            if cell.has_formatted_output()
                                && parts.controls.last().is_none_or(|control| control.expanded)
                            {
                                let offset = parts
                                    .controls
                                    .last()
                                    .map_or(0, |control| layout.line_offset(control.line))
                                    + parts.header.map_or(0, |header| header.source_offset);
                                layout = layout.with_output_controls(offset, output_mode);
                            }
                            for control in parts.controls {
                                layout = layout.with_part_control(control);
                            }
                            layout
                        }
                        // Detailed content and markerless cards share the gutter without acquiring
                        // synthetic markers, disclosure identities, or expansion controls.
                        _ if !disclosure => {
                            let columns = if seat.content == Content::Beheaded {
                                crate::transcript_geometry::NESTED_COLUMNS
                            } else if cell.as_any().is::<crate::history_cell::WorkStatusCell>() {
                                crate::transcript_geometry::TITLE_COLUMN
                                    - crate::transcript_geometry::MARKER_COLUMNS
                            } else {
                                crate::transcript_geometry::GUTTER_COLUMNS
                            };
                            let content_width = if mode == HistoryRenderMode::Rich
                                && cell.as_any().is::<crate::history_cell::WorkStatusCell>()
                            {
                                width.saturating_sub(columns).max(/*other*/ 1)
                            } else {
                                content_width
                            };
                            let mut lines = crate::history_cell::fullscreen_session_lines(
                                cell.as_ref(),
                                content_width,
                                detailed,
                                mode,
                            );
                            if mode == HistoryRenderMode::Rich {
                                if seat.content == Content::Beheaded {
                                    feed::behead(&mut lines);
                                }
                                TextLayout::new(
                                    crate::transcript_geometry::gutter_lines(
                                        crate::transcript_geometry::clock_lines(lines, timestamp),
                                        columns,
                                    ),
                                    width,
                                )
                                .with_header_tokens(
                                    header_tokens,
                                    if detailed {
                                        TokenSeat::Gutter
                                    } else {
                                        TokenSeat::Trailing
                                    },
                                )
                            } else {
                                TextLayout::new(lines, width)
                            }
                        }
                        Content::Beheaded => {
                            let mut lines = activity(expanded, &expanded_parts);
                            yield_head(&mut lines);
                            // Its header was all it showed: the first row of its details that can hold
                            // a control stands in, holding the one that shows the rest.
                            if lines.activity.is_empty() && lines.disclosure.is_some() {
                                let mut details = expanded_parts.clone();
                                details.extend(ids.iter().cloned());
                                let mut unfolded = activity(/*expanded*/ true, &details);
                                yield_head(&mut unfolded);
                                lines.activity = unfolded
                                    .activity
                                    .into_iter()
                                    .find(holds_control)
                                    .into_iter()
                                    .collect();
                            }
                            activity_layout(
                                lines,
                                crate::transcript_geometry::NESTED_COLUMNS,
                                width,
                                expanded,
                            )
                        }
                        Content::Own | Content::Loose => activity_layout(
                            activity(expanded, &expanded_parts),
                            crate::transcript_geometry::GUTTER_COLUMNS,
                            width,
                            expanded,
                        ),
                    }
                };

                if let Some(header) = header {
                    layout = layout.with_fold_header(header.render(
                        &output_tokens,
                        &expanded_parts,
                        output_mode,
                        &invocations,
                        full_content,
                        width,
                    ));
                }
                layout
            };
            if let Some(snapshot) = snapshot {
                Box::new(move || render(&snapshot.restore()))
            } else {
                let layout = render(invocations);
                Box::new(move || layout)
            }
        })
    }

    /// The nearest earlier entry that draws anything.
    fn previous_visible(&mut self, cells: &[Arc<dyn HistoryCell>], index: usize) -> Option<usize> {
        let mut candidate = index.min(cells.len());
        while let Some(previous) = candidate.checked_sub(/*rhs*/ 1) {
            candidate = previous;
            let transcript_only = !self.detailed
                && self.mode == HistoryRenderMode::Rich
                && cells[previous]
                    .as_any()
                    .downcast_ref::<ReasoningSummaryCell>()
                    .is_some_and(ReasoningSummaryCell::is_transcript_only);
            if self.elides_cell(cells, previous)
                || transcript_only
                || !self.feed_seat(cells, previous).is_visible()
            {
                continue;
            }
            return Some(previous);
        }
        None
    }
}

/// Zero for a loose entry drawn as before; otherwise a digest of everything its feed rows show.
fn feed_digest(seat: &Seat, revision: u64, bills: u64) -> u64 {
    if *seat == Seat::LOOSE {
        return 0;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (seat, revision, bills).hash(&mut hasher);
    hasher.finish() | 1
}

impl LayoutCache {
    pub(super) fn frame(&self) -> u64 {
        self.frame
    }

    pub(super) fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(/*rhs*/ 1);
        self.waiting = false;
        self.frame_keys.clear();
        self.pending
            .retain(|pending| pending.requested_frame + 2 >= self.frame);
        let state = RenderState::current();
        if self.render_state.as_ref() != Some(&state) {
            self.clear();
            self.render_state = Some(state);
        }
    }

    pub(super) fn get(
        &mut self,
        cell: &Arc<dyn HistoryCell>,
        width: u16,
        presentation: CellPresentation,
        prepare: impl FnOnce() -> Box<dyn FnOnce() -> TextLayout + Send>,
    ) -> Option<Arc<TextLayout>> {
        let source = Arc::downgrade(cell);
        let revision = RevisionKey {
            identity: Arc::as_ptr(cell).cast::<()>() as usize,
            width,
            presentation,
        };
        let key = *self.frame_keys.entry(revision).or_insert_with(|| {
            let animation_tick = self
                .pending
                .iter()
                .find(|pending| {
                    pending.source.ptr_eq(&source)
                        && pending.width == width
                        && pending.presentation == presentation
                })
                .map_or_else(
                    || cell.transcript_animation_tick(),
                    |pending| pending.animation_tick,
                );
            CacheKey {
                revision,
                animation_tick,
            }
        });
        let animation_tick = key.animation_tick;
        if let Some(mut entry) = self.take(&key).filter(|entry| {
            self.frames.is_some()
                || cell.has_stable_transcript_height()
                || entry.rendered_frame == self.frame
        }) {
            if let Some(frames) = &self.frames
                && !cell.has_stable_transcript_height()
                && animation_tick.is_none()
            {
                if entry.refreshed_at.elapsed() >= std::time::Duration::from_secs(1) {
                    let pending = self.pending.iter().position(|pending| {
                        pending.source.ptr_eq(&source)
                            && pending.width == width
                            && pending.presentation == presentation
                    });
                    let pending = pending.unwrap_or_else(|| {
                        self.pending.push(PendingLayout {
                            source: source.clone(),
                            requested_frame: self.frame,
                            width,
                            presentation,
                            animation_tick,
                            task: {
                                let render = prepare();
                                let previous = Arc::clone(&entry.layout);
                                super::preparation::Pending::new(frames.clone(), move || {
                                    let layout = presentation.decorate(render());
                                    if layout.same_presentation(&previous) {
                                        previous
                                    } else {
                                        Arc::new(layout)
                                    }
                                })
                            },
                        });
                        self.pending.len() - 1
                    });
                    self.pending[pending].requested_frame = self.frame;
                    if let Some(layout) = self.pending[pending].task.poll() {
                        self.pending.remove(pending);
                        entry.layout = layout;
                        entry.refreshed_at = std::time::Instant::now();
                    }
                }
                if entry.rendered_frame != self.frame {
                    frames.schedule_content_in(std::time::Duration::from_secs(1));
                }
            }
            entry.rendered_frame = self.frame;
            let layout = Arc::clone(&entry.layout);
            self.insert(key, entry);
            return Some(layout);
        }
        let layout = if let Some(frames) = &self.frames {
            self.pending
                .retain(|entry| entry.source.strong_count() != 0 && entry.width == width);
            let index = self.pending.iter().position(|entry| {
                entry.source.ptr_eq(&source)
                    && entry.width == width
                    && entry.presentation == presentation
                    && entry.animation_tick == animation_tick
            });
            let index = index.unwrap_or_else(|| {
                self.pending.push(PendingLayout {
                    source: source.clone(),
                    requested_frame: self.frame,
                    width,
                    presentation,
                    animation_tick,
                    task: {
                        let render = prepare();
                        super::preparation::Pending::new(frames.clone(), move || {
                            Arc::new(presentation.decorate(render()))
                        })
                    },
                });
                self.pending.len() - 1
            });
            self.pending[index].requested_frame = self.frame;
            let Some(layout) = self.pending[index].task.poll() else {
                self.waiting = true;
                return None;
            };
            self.pending.remove(index);
            layout
        } else {
            Arc::new(presentation.decorate(prepare()()))
        };
        self.insert(
            key,
            CachedLayout {
                access: 0,
                source,
                layout: Arc::clone(&layout),
                rendered_frame: self.frame,
                refreshed_at: std::time::Instant::now(),
            },
        );
        self.evict();
        Some(layout)
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.recency.clear();
        self.frame_keys.clear();
        self.bytes = 0;
        self.pending.clear();
        self.rewraps.clear();
    }

    pub(super) fn invalidate_call(&mut self, call_id: &str) {
        self.pending.clear();
        let keys: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(key, entry)| {
                let retained = entry.source.upgrade().is_some_and(|cell| {
                    !crate::history_cell::invocation::collect(cell.as_ref())
                        .iter()
                        .any(|call| call.id == call_id)
                });
                (!retained).then_some(*key)
            })
            .collect();
        for key in keys {
            self.take(&key);
        }
    }

    fn take(&mut self, key: &CacheKey) -> Option<CachedLayout> {
        let entry = self.entries.remove(key)?;
        self.recency.remove(&entry.access);
        self.bytes -= entry.layout.retained_bytes();
        Some(entry)
    }

    fn insert(&mut self, key: CacheKey, mut entry: CachedLayout) {
        self.access += 1;
        entry.access = self.access;
        self.bytes += entry.layout.retained_bytes();
        self.recency.insert(entry.access, key);
        self.entries.insert(key, entry);
    }

    fn evict(&mut self) {
        while self.entries.len() > 1 && self.bytes > MAX_CACHED_TEXT_BYTES {
            let Some((_, key)) = self.recency.first_key_value() else {
                break;
            };
            let key = *key;
            if self.entries[&key].rendered_frame + 1 >= self.frame {
                break;
            }
            self.take(&key);
        }
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
