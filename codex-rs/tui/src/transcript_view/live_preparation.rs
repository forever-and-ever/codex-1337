//! Live sources enter preparation as owned revisions, never borrowed widget callbacks.

use super::ActiveCellTranscriptKey;
use super::ActivityTranscriptLines;
use super::Arc;
use super::HistoryCell;
use super::HistoryRenderMode;
use super::HyperlinkLine;
use super::TextLayout;
use super::TranscriptView;
use super::feed;
use super::invocation;
use super::invocation::PreparationSource;
use super::layout;
use super::layout::RenderState;
use super::preparation::Pending;
use super::tool_output_tokens;
use crate::history_cell::activity_parts::FoldState;
use crate::history_cell::output_provenance::OutputMode;

#[derive(Clone, Debug)]
pub(crate) struct OwnedLiveTail {
    pub(crate) active: Option<Arc<dyn HistoryCell>>,
    pub(crate) auxiliary: Vec<Arc<dyn HistoryCell>>,
}

#[derive(Clone, PartialEq, Eq)]
struct Key {
    width: u16,
    source: ActiveCellTranscriptKey,
    detailed: bool,
    mode: HistoryRenderMode,
    render: RenderState,
    expanded: FoldState,
    output_mode: OutputMode,
    tokens: Option<u64>,
    invocations: u64,
    search: bool,
}

struct Layouts {
    plain: Arc<TextLayout>,
    separated: Arc<TextLayout>,
    search: Option<Arc<TextLayout>>,
    activity_ids: Arc<[String]>,
}

#[derive(Default)]
pub(super) struct LivePreparation {
    source: Option<(u64, u16, OwnedLiveTail)>,
    pending: Option<(Key, Pending<Layouts>)>,
    ready: Option<(Key, Arc<[String]>)>,
}

impl LivePreparation {
    pub(super) fn activity_ids(&self) -> Option<Arc<[String]>> {
        self.ready.as_ref().map(|(_, ids)| Arc::clone(ids))
    }
}

impl OwnedLiveTail {
    fn lines(
        &self,
        width: u16,
        detailed: bool,
        mode: HistoryRenderMode,
        active: bool,
    ) -> Vec<HyperlinkLine> {
        let mut previous = None;
        let mut lines = Vec::new();
        for cell in self
            .active
            .iter()
            .filter(|_| active)
            .chain(self.auxiliary.iter())
        {
            let body = if crate::history_cell::is_message(cell.as_ref()) {
                crate::history_cell::fullscreen_session_lines(cell.as_ref(), width, detailed, mode)
            } else if detailed {
                cell.transcript_hyperlink_lines(width)
            } else if cell.as_any().is::<crate::history_cell::SessionInfoCell>() {
                crate::history_cell::fullscreen_session_lines(cell.as_ref(), width, false, mode)
            } else if mode == HistoryRenderMode::Raw {
                cell.display_hyperlink_lines_for_mode(width, mode)
            } else {
                cell.compact_hyperlink_lines(width)
            };
            if body.is_empty() {
                continue;
            }
            if crate::transcript_geometry::separated(
                previous,
                cell.is_tool_entry(),
                cell.is_stream_continuation(),
            ) {
                lines.push(HyperlinkLine::from(""));
            }
            lines.extend(crate::transcript_geometry::clock_lines(
                body,
                cell.transcript_timestamp(),
            ));
            previous = Some(cell.as_ref());
        }
        lines
    }

    fn render(
        &self,
        key: &Key,
        expanded: bool,
        invocations: &invocation::Invocations,
        tokens: &tool_output_tokens::ToolOutputTokens,
        search: bool,
    ) -> TextLayout {
        for cell in self.active.iter().chain(&self.auxiliary) {
            crate::history_cell::prepare_links(cell.as_ref());
        }
        let width = key.width;
        let columns = if self
            .active
            .as_deref()
            .is_some_and(crate::history_cell::is_message)
        {
            crate::transcript_geometry::TITLE_COLUMN
        } else {
            crate::transcript_geometry::GUTTER_COLUMNS
        };
        let content_width = if key.mode == HistoryRenderMode::Rich {
            width.saturating_sub(columns).max(1)
        } else {
            width
        };
        if key.detailed {
            let lines = self.lines(content_width, true, key.mode, true);
            return TextLayout::new(
                if key.mode == HistoryRenderMode::Rich {
                    crate::transcript_geometry::gutter_lines(lines, columns)
                } else {
                    lines
                },
                width,
            );
        }
        let active = self.active.as_deref();
        let mut auxiliary = self.lines(content_width, false, key.mode, false);
        if key.mode == HistoryRenderMode::Rich
            && let Some(cell) = active
            && let Some(layout) = invocations.layout(
                cell,
                PreparationSource::Live(key.source.revision),
                width,
                true,
                feed::Content::Loose,
                search,
                tokens,
                &key.expanded,
                auxiliary.clone(),
            )
        {
            return layout;
        }
        let has_activity = key.mode == HistoryRenderMode::Rich
            && active.is_some_and(|cell| !cell.activity_ids().is_empty());
        let parts = active
            .filter(|_| has_activity)
            .and_then(|cell| cell.output_parts(content_width, 0, &key.expanded, key.output_mode));
        let (activity, header, disclosure, controls) = if let Some(parts) = parts {
            (
                crate::transcript_geometry::clock_lines(
                    parts.lines,
                    active.and_then(HistoryCell::transcript_timestamp),
                ),
                parts.header,
                parts.output,
                parts.controls,
            )
        } else {
            let activity = active.map_or_else(Vec::new, |cell| {
                let lines = if expanded && has_activity {
                    cell.expanded_output_lines(content_width, key.output_mode)
                } else if crate::history_cell::is_message(cell)
                    || cell.as_any().is::<crate::history_cell::SessionInfoCell>()
                {
                    crate::history_cell::fullscreen_session_lines(
                        cell,
                        content_width,
                        false,
                        key.mode,
                    )
                } else if key.mode == HistoryRenderMode::Raw {
                    cell.display_hyperlink_lines_for_mode(content_width, key.mode)
                } else {
                    cell.compact_hyperlink_lines(content_width)
                };
                crate::transcript_geometry::clock_lines(lines, cell.transcript_timestamp())
            });
            (
                activity,
                active.and_then(|cell| {
                    if expanded && has_activity {
                        cell.expanded_activity_header()
                    } else {
                        cell.activity_header()
                    }
                }),
                active
                    .filter(|_| has_activity)
                    .and_then(|cell| cell.activity_disclosure(content_width)),
                Vec::new(),
            )
        };
        if !activity.is_empty() && !auxiliary.is_empty() {
            auxiliary.insert(0, HyperlinkLine::from(""));
        }
        let lines = ActivityTranscriptLines {
            output_mode: (expanded && active.is_some_and(HistoryCell::has_formatted_output))
                .then_some(key.output_mode),
            is_tool_entry: active.is_some_and(HistoryCell::is_tool_entry),
            activity,
            header,
            auxiliary,
            header_tokens: active
                .and_then(HistoryCell::activity_header_tokens)
                .or(key.tokens),
            disclosure,
            controls,
        };
        if key.mode == HistoryRenderMode::Rich {
            layout::activity_layout(lines, columns, width, expanded)
        } else {
            let mut lines = lines;
            lines.activity.extend(lines.auxiliary);
            TextLayout::new(lines.activity, width)
        }
    }
}

impl TranscriptView {
    #[expect(
        clippy::expect_used,
        reason = "A completed task still owns its pending revision, and the requested live snapshot was installed before use."
    )]
    pub(crate) fn sync_owned_live_tail(
        &mut self,
        width: u16,
        source: Option<ActiveCellTranscriptKey>,
        expanded: bool,
        snapshot: impl FnOnce() -> OwnedLiveTail,
    ) -> Option<bool> {
        let frames = self.cache.frames.clone()?;
        let Some(source) = source else {
            self.live_preparation = LivePreparation::default();

            self.live_key = None;
            self.search.live = None;
            self.invalidate_live_search();
            self.live_separated = None;
            return Some(self.live.take().is_some());
        };
        let key = Key {
            width,
            source,
            detailed: self.detailed,
            mode: self.mode,
            render: RenderState::current(),
            expanded: self.disclosure.expanded.scoped(self.live_fold_scope()),
            output_mode: self.disclosure.output_mode(&self.disclosure.live_ids),
            tokens: self.activity_output_tokens(&self.disclosure.live_ids),
            invocations: self.invocations.revision(),
            search: self.search.is_active(),
        };
        if let Some((pending, task)) = &mut self.live_preparation.pending {
            if pending.width != width || pending.render != key.render {
                self.live_preparation.pending = None;
            } else if let Some(layouts) = task.poll() {
                let (completed, _) = self
                    .live_preparation
                    .pending
                    .take()
                    .expect("completed live preparation");
                let revision_changed =
                    self.live_key.map(|(_, key)| key.revision) != Some(completed.source.revision);
                if revision_changed {
                    if !self.is_following() && layouts.plain.row_count() != 0 {
                        self.unseen_activity = true;
                    }
                    self.invalidate_live_search();
                }
                self.live_key = Some((width, completed.source));
                self.live_continuation = completed.source.is_stream_continuation;
                self.live_tool_entry = completed.source.is_tool_entry;
                self.live = (layouts.plain.row_count() != 0).then_some(layouts.plain);
                self.live_separated = Some(layouts.separated);
                self.search.live = layouts.search;

                self.live_preparation.ready = Some((completed, layouts.activity_ids));
                // Finish the in-flight revision before accepting a newer one. Continuous
                // streaming cannot starve presentation by cancelling every completion.
                if self.live_preparation.ready.as_ref().map(|(key, _)| key) != Some(&key) {
                    frames.schedule_frame();
                }
                return Some(true);
            } else {
                return Some(false);
            }
        }
        if self.live_preparation.ready.as_ref().map(|(key, _)| key) == Some(&key) {
            return Some(false);
        }
        if self
            .live_preparation
            .source
            .as_ref()
            .is_none_or(|(revision, source_width, _)| {
                *revision != source.revision || *source_width != width
            })
        {
            self.live_preparation.source = Some((source.revision, width, snapshot()));
        }
        let tail = self
            .live_preparation
            .source
            .as_ref()
            .expect("live snapshot")
            .2
            .clone();
        let invocations = self
            .invocations
            .snapshot(tail.active.iter().chain(&tail.auxiliary).map(AsRef::as_ref));
        let token_ids: Vec<_> = tail
            .active
            .iter()
            .chain(&tail.auxiliary)
            .flat_map(|cell| cell.activity_ids())
            .collect();
        let tokens = self.tool_output_tokens.subset(&token_ids);
        let search = self.search.is_active();
        let requested = key.clone();
        let mut task = Pending::new(frames, move || {
            let invocations = invocations.restore();
            let plain = tail.render(&key, expanded, &invocations, &tokens, false);
            let separated = Arc::new(plain.rewrap(width).with_leading_separator());
            let search =
                search.then(|| Arc::new(tail.render(&key, true, &invocations, &tokens, true)));
            Layouts {
                activity_ids: tail
                    .active
                    .as_ref()
                    .map_or_else(Vec::new, |cell| cell.activity_ids())
                    .into(),
                plain: Arc::new(plain),
                separated,
                search,
            }
        });
        task.submit();
        self.live_preparation.pending = Some((requested, task));

        Some(false)
    }
}
