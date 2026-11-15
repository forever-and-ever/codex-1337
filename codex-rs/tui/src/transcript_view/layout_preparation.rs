//! Cached immutable layouts and the owned closure submitted to background preparation.

use super::ActivityTranscriptLines;
use super::Arc;
use super::CellPresentation;
use super::Content;
use super::EntryKey;
use super::Hash;
use super::HistoryCell;
use super::HistoryRenderMode;
use super::RenderState;
use super::Seat;
use super::TextLayout;
use super::TokenSeat;
use super::TranscriptView;
use super::activity_layout;
use super::feed;
use super::feed_digest;
use super::holds_control;
use super::invocation;
use super::rewrap;
use super::yield_head;
use crate::history_cell::ToolOutputGroup;
use crate::history_cell::activity_parts::FoldState;
use std::sync::Weak;

const MAX_CACHED_TEXT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
pub(in crate::transcript_view) struct LayoutCache {
    entries: std::collections::HashMap<CacheKey, CachedLayout>,
    frame_keys: std::collections::HashMap<RevisionKey, CacheKey>,
    recency: std::collections::BTreeMap<u64, CacheKey>,
    access: u64,
    bytes: usize,
    frame: u64,
    render_state: Option<RenderState>,
    pub(in crate::transcript_view) frames: Option<crate::tui::FrameRequester>,
    pending: Vec<PendingLayout>,
    pub(in crate::transcript_view) waiting: bool,
    pub(super) rewraps: Vec<rewrap::Rewrap>,
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
    task: crate::transcript_view::preparation::Pending<Arc<TextLayout>>,
}

impl LayoutCache {
    pub(in crate::transcript_view) fn frame(&self) -> u64 {
        self.frame
    }

    pub(in crate::transcript_view) fn begin_frame(&mut self) {
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

    pub(in crate::transcript_view) fn get(
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
                                crate::transcript_view::preparation::Pending::new(
                                    frames.clone(),
                                    move || {
                                        let layout = presentation.decorate(render());
                                        if layout.same_presentation(&previous) {
                                            previous
                                        } else {
                                            Arc::new(layout)
                                        }
                                    },
                                )
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
                        crate::transcript_view::preparation::Pending::new(
                            frames.clone(),
                            move || Arc::new(presentation.decorate(render())),
                        )
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

    pub(in crate::transcript_view) fn clear(&mut self) {
        self.entries.clear();
        self.recency.clear();
        self.frame_keys.clear();
        self.bytes = 0;
        self.pending.clear();
        self.rewraps.clear();
    }

    pub(in crate::transcript_view) fn invalidate_call(&mut self, call_id: &str) {
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

impl TranscriptView {
    #[expect(
        clippy::expect_used,
        reason = "Feed body placement is assigned only to entries with a feed body."
    )]
    pub(in crate::transcript_view) fn prepare_entry_layout(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        full_content: bool,
    ) -> Option<Arc<TextLayout>> {
        if index > cells.len() {
            return None;
        }
        self.refresh_feed(cells, index);
        if !full_content && self.eclipsed(cells, index) {
            return Some(Arc::new(TextLayout::new(
                Vec::new(),
                self.area.width.max(1),
            )));
        }
        // Search reads retained entry content independently of its manual folds.
        let seat = if full_content
            && (self.eclipsed(cells, index)
                || cells
                    .get(index)
                    .is_some_and(|cell| feed::feed_entry(cell.as_ref()).is_some()))
        {
            Seat::LOOSE
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
        let manually_expanded = self.feed.activity_group(index).map_or_else(
            || self.disclosure.is_expanded(&ids),
            |group| {
                ids.iter().any(|id| {
                    self.disclosure.expanded.is_open_in(
                        &crate::history_cell::activity_parts::FoldScope::Transcript,
                        group,
                        id,
                        false,
                    )
                })
            },
        );
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
        // Adjacent visible rich prompts share their ground; hidden work does not split it.
        let shares_prompt_background = mode == HistoryRenderMode::Rich
            && cell.as_any().is::<crate::history_cell::UserHistoryCell>()
            && previous.is_some_and(|previous| {
                previous
                    .as_any()
                    .is::<crate::history_cell::UserHistoryCell>()
            });
        // Dialogue mode stacks prompts and answers; with the work shown, blank rows set them apart.
        let separated = !self.dialogue_only()
            && !shares_prompt_background
            && match &seat.header {
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
            let edits = feed.edit_sources(cells, index);
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
            let expanded_parts = expanded_parts.scoped(feed.fold_scope(index));
            let render = move |invocations: &invocation::Invocations| {
                crate::history_cell::prepare_links(cell.as_ref());
                if cell
                    .as_any()
                    .is::<crate::rendered_selection::RenderedSurface>()
                {
                    return TextLayout::new(cell.display_hyperlink_lines(width), width);
                }
                let mut header = header;
                let content = if header.as_mut().is_some_and(|header| {
                    header.keep_empty_code_mode_open(
                        &cell,
                        invocations,
                        &expanded_parts,
                        full_content,
                        width,
                    )
                }) {
                    Content::Beheaded
                } else {
                    seat.content
                };
                let content_width = crate::transcript_geometry::content_width(width, mode);
                // A member's clock is its item's header's.
                let timestamp = if loose {
                    cell.transcript_timestamp()
                } else {
                    None
                };
                // The activity's rows, compact or expanded, and what controls them.
                let activity = |expanded: bool, expanded_parts: &FoldState| {
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
                let invocation =
                    (mode == HistoryRenderMode::Rich && !detailed && content != Content::Hidden)
                        .then(|| {
                            invocations.layout_scoped(
                                cell.as_ref(),
                                edits.iter().map(AsRef::as_ref),
                                invocation::PreparationSource::Retained(&cell),
                                width,
                                input_open,
                                content,
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
                    match content {
                        Content::Hidden => TextLayout::new(Vec::new(), width),
                        Content::Thought => TextLayout::new(
                            crate::transcript_geometry::rich_lines(
                                cell.expanded_hyperlink_lines(content_width),
                            ),
                            width,
                        ),
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
                            let columns = if content == Content::Beheaded {
                                crate::transcript_geometry::NESTED_COLUMNS
                            } else if cell.as_any().is::<crate::history_cell::RespondingCell>()
                                || crate::history_cell::is_message(cell.as_ref())
                            {
                                crate::transcript_geometry::TITLE_COLUMN
                            } else if cell.as_any().is::<crate::history_cell::WorkStatusCell>()
                                || cell.as_any().is::<crate::history_cell::NotificationCell>()
                                || cell.as_any().is::<crate::history_cell::SessionNoticeCell>()
                            {
                                crate::transcript_geometry::TITLE_COLUMN
                                    - crate::transcript_geometry::MARKER_COLUMNS
                            } else {
                                crate::transcript_geometry::GUTTER_COLUMNS
                            };
                            let content_width = if mode == HistoryRenderMode::Rich
                                && (cell.as_any().is::<crate::history_cell::WorkStatusCell>()
                                    || cell.as_any().is::<crate::history_cell::NotificationCell>()
                                    || cell.as_any().is::<crate::history_cell::SessionNoticeCell>()
                                    || crate::history_cell::is_message(cell.as_ref()))
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
                                if content == Content::Beheaded {
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
                        invocations,
                        full_content,
                        width,
                    ));
                }
                layout.with_brackets(mode == HistoryRenderMode::Rich)
            };
            if let Some(snapshot) = snapshot {
                Box::new(move || render(&snapshot.restore()))
            } else {
                let layout = render(invocations);
                Box::new(move || layout)
            }
        })
    }
}
