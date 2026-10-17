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
use crate::history_cell::output_provenance::OutputMode;
use crate::transcript_geometry::TokenSeat;
use std::collections::HashSet;
use std::hash::Hash;
use std::hash::Hasher;

use super::*;

#[path = "layout_rewrap.rs"]
mod rewrap;

#[path = "layout_preparation.rs"]
mod preparation;
pub(super) use preparation::LayoutCache;
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
        self.prepare_entry_layout(cells, index, full_content)
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
            if cells[previous]
                .as_any()
                .is::<crate::history_cell::StreamAnchorCell>()
                || self.elides_cell(cells, previous)
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

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
