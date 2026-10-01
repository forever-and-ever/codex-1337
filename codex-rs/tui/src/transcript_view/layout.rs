//! Width-specific layouts retained only for recently displayed conversation entries.
//!
//! Mutable cells refresh each frame, then share that frame's layout across measurement and paint.
//! Stable cells also invalidate when animation ticks, syntax themes, or terminal colors change.

use crate::history_cell::ActivityDisclosure;
use crate::history_cell::ReasoningSummaryCell;
use crate::history_cell::ToolOutputGroup;
use crate::key_hint::ShortcutHint;
use crate::keymap::KeymapContext;
use std::sync::Weak;

use super::*;

const MAX_CACHED_ENTRIES: usize = 64;
const MAX_CACHED_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// The selected activity presentation and any live content that follows it.
pub(crate) struct ActivityTranscriptLines {
    pub(crate) activity: Vec<HyperlinkLine>,
    pub(crate) auxiliary: Vec<HyperlinkLine>,
    pub(crate) tool_output_tokens: Option<u64>,
    pub(crate) disclosure: Option<ActivityDisclosure>,
}

/// Keep disclosure controls outside selectable source without rendering hidden details.
pub(super) fn activity_layout(
    mut lines: ActivityTranscriptLines,
    width: u16,
    expanded: bool,
    _shortcut: Option<ShortcutHint>,
) -> TextLayout {
    let control_line = match lines.disclosure {
        Some(ActivityDisclosure::EditedFile { header }) => usize::from(header),
        _ => 0,
    };
    let mut heading_width = 0;
    if let Some(header) = lines.activity.first() {
        for span in &header.line.spans {
            let action = span.content.split_whitespace().next().unwrap_or_default();
            if span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
                && action.chars().any(char::is_alphabetic)
            {
                heading_width += crate::width::display_width(action);
                break;
            }
            heading_width += span.width();
        }
    }
    let estimate = lines
        .tool_output_tokens
        .map(|tokens| {
            let tokens = codex_protocol::num_format::format_with_separators(
                i64::try_from(tokens).unwrap_or(i64::MAX),
            );
            format!(" (~ {tokens} tokens)")
        })
        .filter(|label| label.len() + 4 + heading_width + 3 <= usize::from(width));
    // Reserve the estimate before clipping, so invocation arguments never wrap collapsed.
    if !expanded {
        for (index, header) in lines.activity.iter_mut().enumerate() {
            let estimate_width = if index == 0 {
                estimate.as_ref().map_or(0, String::len)
            } else {
                0
            };
            let available = usize::from(width).saturating_sub(4 + estimate_width);
            if header.width() > available {
                lines.disclosure.get_or_insert(ActivityDisclosure::Generic);
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
                *header = line.into();
                header.source = Some(source);
            }
        }
    }
    let has_details = !lines.activity.is_empty() && (expanded || lines.disclosure.is_some());
    lines.activity = crate::terminal_hyperlinks::prefix_hyperlink_lines(
        lines.activity,
        "    ".into(),
        "    ".into(),
    );
    lines.activity.extend(lines.auxiliary);
    let mut layout = TextLayout::new(lines.activity, width);
    if let Some(estimate) = estimate {
        layout = if matches!(
            lines.disclosure,
            Some(ActivityDisclosure::EditedFile { header: false })
        ) {
            // Keep each call's bill on its file row when the shared Edited heading is omitted.
            let offset = layout.text().find('\n').unwrap_or(layout.text().len());
            layout.with_estimate_at(offset, estimate)
        } else {
            layout.with_activity_estimate(estimate)
        };
    }
    if !has_details {
        return layout;
    }
    layout
        .with_leading_disclosure_control(if expanded { "[-]" } else { "[+]" }.into(), control_line)
}

#[derive(Default)]
pub(super) struct LayoutCache {
    entries: Vec<CachedLayout>,
    frame: u64,
    render_state: Option<RenderState>,
}

#[derive(PartialEq, Eq)]
struct RenderState {
    theme: u64,
    foreground: Option<(u8, u8, u8)>,
    background: Option<(u8, u8, u8)>,
    color_level: crate::terminal_palette::StdoutColorLevel,
}

struct CachedLayout {
    source: Weak<dyn HistoryCell>,
    width: u16,
    presentation: CellPresentation,
    layout: Arc<TextLayout>,
    rendered_frame: u64,
    animation_tick: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct CellPresentation {
    separated: bool,
    turn_tip_space: bool,
    expanded: bool,
    disclosure: bool,
    continued_edit: bool,
}

impl TranscriptView {
    pub(crate) fn set_output_estimates_enabled(&mut self, enabled: bool) {
        if self.output_estimates_enabled != enabled {
            self.output_estimates_enabled = enabled;
            self.cache.clear();
            self.live_key = None;
        }
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
        if let Some(layout) = self
            .snapshot()
            .and_then(|snapshot| snapshot.pinned.get(&key))
        {
            return Some(Arc::clone(layout));
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
        let previous = cells.get(..index)?.iter().rev().find(|cell| {
            self.detailed
                || self.mode != HistoryRenderMode::Rich
                || !cell
                    .as_any()
                    .downcast_ref::<ReasoningSummaryCell>()
                    .is_some_and(ReasoningSummaryCell::is_transcript_only)
        });
        let Some(cell) = cells.get(index) else {
            if index != cells.len() {
                return None;
            }
            let live = self.live.as_ref()?;
            let follows_tool_entry = !self.disclosure.live_ids.is_empty()
                && previous.is_some_and(|cell| !cell.activity_ids().is_empty());
            if cells.is_empty() || self.live_continuation || follows_tool_entry {
                return Some(Arc::clone(live));
            }
            let width = self.area.width.max(/*other*/ 1);
            return Some(Arc::clone(self.live_separated.get_or_insert_with(|| {
                Arc::new(live.rewrap(width).with_leading_separator())
            })));
        };
        let width = self.area.width.max(/*other*/ 1);
        // Session information belongs before all history, including pages not loaded yet.
        // Pinned displayed revisions above remain visible; hidden headers never enter the cache.
        if self.history.has_unloaded_history()
            && cell.as_any().is::<crate::history_cell::SessionInfoCell>()
        {
            return Some(Arc::new(TextLayout::new(Vec::new(), width)));
        }
        let detailed = self.detailed;
        let mode = self.mode;
        let ids = cell.activity_ids();
        let disclosure = !detailed && mode == HistoryRenderMode::Rich && !ids.is_empty();
        let expanded = disclosure && self.disclosure.is_expanded(&ids);
        let tool_output_tokens = self.activity_output_tokens(&ids);
        let continued_edit = disclosure
            && !cell.is_stream_continuation()
            && ToolOutputGroup::is_patch(cell.as_ref())
            && previous.is_some_and(|previous| ToolOutputGroup::is_patch(previous.as_ref()));
        let follows_tool_entry =
            !ids.is_empty() && previous.is_some_and(|cell| !cell.activity_ids().is_empty());
        if expanded {
            self.disclosure.expanded.extend(ids);
        }
        let separated = index > 0 && !cell.is_stream_continuation() && !follows_tool_entry;
        let presentation = CellPresentation {
            separated,
            turn_tip_space: self.turn_tip_key == Some(EntryKey::cell(cell)),
            expanded,
            disclosure,
            continued_edit,
        };
        let shortcut = self
            .disclosure
            .keymap
            .primary_hint(KeymapContext::Global, "open_transcript");
        Some(self.cache.get(cell, width, presentation, || {
            if disclosure {
                let mut activity = if expanded {
                    cell.expanded_hyperlink_lines(width)
                } else {
                    cell.compact_hyperlink_lines(width)
                };
                let disclosure = if continued_edit {
                    activity.remove(0);
                    Some(ActivityDisclosure::EditedFile { header: false })
                } else {
                    cell.activity_disclosure(width)
                };
                activity_layout(
                    ActivityTranscriptLines {
                        activity,
                        auxiliary: Vec::new(),
                        tool_output_tokens,
                        disclosure,
                    },
                    width,
                    expanded,
                    shortcut,
                )
            } else {
                TextLayout::new(
                    crate::history_cell::fullscreen_session_lines(
                        cell.as_ref(),
                        width,
                        detailed,
                        mode,
                    ),
                    width,
                )
            }
        }))
    }
}

impl LayoutCache {
    pub(super) fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(/*rhs*/ 1);
        let state = RenderState {
            theme: crate::render::highlight::syntax_theme_revision(),
            foreground: crate::terminal_palette::default_fg(),
            background: crate::terminal_palette::default_bg(),
            color_level: crate::terminal_palette::stdout_color_level(),
        };
        if self.render_state.as_ref() != Some(&state) {
            self.entries.clear();
            self.render_state = Some(state);
        }
    }

    pub(super) fn get(
        &mut self,
        cell: &Arc<dyn HistoryCell>,
        width: u16,
        presentation: CellPresentation,
        render: impl FnOnce() -> TextLayout,
    ) -> Arc<TextLayout> {
        let source = Arc::downgrade(cell);
        let animation_tick = cell.transcript_animation_tick();
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.width == width
                && entry.presentation == presentation
                && entry.source.ptr_eq(&source)
                && (entry.rendered_frame == self.frame
                    || (cell.has_stable_transcript_height()
                        && entry.animation_tick == animation_tick))
        }) {
            let layout = Arc::clone(&self.entries[index].layout);
            if index + 1 != self.entries.len() {
                let entry = self.entries.remove(index);
                self.entries.push(entry);
            }
            return layout;
        }
        let layout = render();
        let layout = if presentation.separated {
            layout.with_leading_separator()
        } else {
            layout
        };
        let layout = Arc::new(if presentation.turn_tip_space {
            layout.with_leading_spacer()
        } else {
            layout
        });
        self.entries.retain(|entry| !entry.source.ptr_eq(&source));
        self.entries.push(CachedLayout {
            source,
            width,
            presentation,
            layout: Arc::clone(&layout),
            rendered_frame: self.frame,
            animation_tick,
        });
        self.evict();
        layout
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    fn evict(&mut self) {
        let mut bytes = self
            .entries
            .iter()
            .map(|entry| entry.layout.text().len())
            .sum::<usize>();
        // Retain a single oversized entry rather than repeatedly laying it out while visible.
        while self.entries.len() > 1
            && (self.entries.len() > MAX_CACHED_ENTRIES || bytes > MAX_CACHED_TEXT_BYTES)
        {
            bytes -= self.entries.remove(/*index*/ 0).layout.text().len();
        }
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
