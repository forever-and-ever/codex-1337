//! Immutable worker chunks are truthful only at their prepared width.
//!
//! Emission transfers Arc slices, never walks rows. Prefix sums address their
//! concatenation; styling/materialization belongs to the existing layout worker.

use super::ActivityDisclosure;
use super::AgentMessageCell;
use super::HistoryCell;
use super::HistoryRenderMode;
use super::HistoryTimestamp;
use super::HyperlinkLine;
use super::Line;
use super::Stylize;
use super::new_proposed_plan_stream;
use super::prefix_proposed_plan_lines;
use super::proposed_plan_style;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct RowChunk {
    pub(crate) rows: Arc<[HyperlinkLine]>,
    pub(crate) range: Range<usize>,
}

#[derive(Debug, Default)]
pub(crate) struct SharedRows {
    chunks: Vec<RowChunk>,
    ends: Vec<usize>,
}

impl SharedRows {
    pub(crate) fn push(&mut self, chunk: RowChunk) {
        let end = self.len() + chunk.range.len();
        self.chunks.push(chunk);
        self.ends.push(end);
    }

    pub(crate) fn len(&self) -> usize {
        self.ends.last().copied().unwrap_or(0)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn row(&self, index: usize) -> &HyperlinkLine {
        let chunk = self.ends.partition_point(|end| *end <= index);
        let start = if chunk == 0 { 0 } else { self.ends[chunk - 1] };
        &self.chunks[chunk].rows[self.chunks[chunk].range.start + index - start]
    }

    pub(crate) fn materialize(&self) -> Vec<HyperlinkLine> {
        (0..self.len())
            .map(|index| self.row(index).clone())
            .collect()
    }
}

#[derive(Debug)]
pub(crate) enum RowProjection {
    Agent {
        first: bool,
        timestamp: Option<HistoryTimestamp>,
    },
    Plan {
        first: bool,
        top: bool,
        bottom: bool,
    },
}

#[derive(Debug)]
pub(crate) enum StreamRowContent {
    Prepared(Box<dyn HistoryCell>),
    Shared {
        rows: SharedRows,
        projection: RowProjection,
    },
}

#[derive(Debug)]
pub(crate) struct StreamRowsCell {
    pub(crate) mode: HistoryRenderMode,
    pub(crate) width: Option<usize>,
    pub(crate) reserved: usize,
    pub(crate) item_id: Option<String>,
    pub(crate) content: StreamRowContent,
}

impl StreamRowsCell {
    fn project(&self) -> Box<dyn HistoryCell> {
        let StreamRowContent::Shared { rows, projection } = &self.content else {
            unreachable!("prepared rows borrow their projection");
        };
        match *projection {
            RowProjection::Agent { first, timestamp } => {
                let mut cell = AgentMessageCell::new_hyperlink_lines(rows.materialize(), first);
                cell.timestamp = timestamp;
                cell.item_id.clone_from(&self.item_id);
                Box::new(cell)
            }
            RowProjection::Plan { first, top, bottom } => {
                let mut cell = new_proposed_plan_stream(
                    render_plan_rows(rows.materialize(), first, top, bottom),
                    !first,
                );
                cell.item_id.clone_from(&self.item_id);
                Box::new(cell)
            }
        }
    }

    fn with_projection<T>(&self, apply: impl FnOnce(&dyn HistoryCell) -> T) -> T {
        match &self.content {
            StreamRowContent::Prepared(body) => apply(body.as_ref()),
            StreamRowContent::Shared { .. } => apply(self.project().as_ref()),
        }
    }
}

impl HistoryCell for StreamRowsCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.with_projection(|body| body.display_lines(width))
    }
    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.with_projection(|body| body.display_hyperlink_lines(width))
    }
    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.with_projection(|body| body.compact_hyperlink_lines(width))
    }
    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.with_projection(|body| body.transcript_hyperlink_lines(width))
    }
    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.with_projection(HistoryCell::raw_lines)
    }
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        match &self.content {
            StreamRowContent::Prepared(body) => body.transcript_timestamp(),
            StreamRowContent::Shared {
                projection: RowProjection::Agent { timestamp, .. },
                ..
            } => *timestamp,
            StreamRowContent::Shared { .. } => None,
        }
    }
    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        None
    }
    fn is_stream_continuation(&self) -> bool {
        match &self.content {
            StreamRowContent::Prepared(body) => body.is_stream_continuation(),
            StreamRowContent::Shared {
                projection: RowProjection::Agent { first, .. } | RowProjection::Plan { first, .. },
                ..
            } => !first,
        }
    }
}

pub(crate) fn render_plan_rows(
    lines: Vec<HyperlinkLine>,
    first: bool,
    top: bool,
    bottom: bool,
) -> Vec<HyperlinkLine> {
    let mut out = Vec::new();
    if first {
        out.push(HyperlinkLine::new(
            vec!["• ".dim(), "Proposed Plan".bold()].into(),
        ));
        out.push(HyperlinkLine::new(Line::from(" ")));
    }
    let mut plan = Vec::new();
    if top {
        plan.push(HyperlinkLine::new(Line::from(" ")));
    }
    plan.extend(lines);
    if bottom {
        plan.push(HyperlinkLine::new(Line::from(" ")));
    }
    out.extend(
        prefix_proposed_plan_lines(plan)
            .into_iter()
            .map(|line| line.style(proposed_plan_style())),
    );
    out
}
