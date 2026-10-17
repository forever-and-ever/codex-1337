//! Immutable rows are truthful only at the width for which the stream worker prepared them.

use super::*;

#[derive(Debug)]
pub(crate) struct StreamRowsCell {
    pub(crate) mode: HistoryRenderMode,
    pub(crate) width: Option<usize>,
    pub(crate) reserved: usize,
    pub(crate) item_id: Option<String>,
    pub(crate) body: Box<dyn HistoryCell>,
}

impl HistoryCell for StreamRowsCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.body.display_lines(width)
    }
    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.body.display_hyperlink_lines(width)
    }
    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.body.compact_hyperlink_lines(width)
    }
    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.body.transcript_hyperlink_lines(width)
    }
    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.body.raw_lines()
    }
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.body.transcript_timestamp()
    }
    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        None
    }
    fn is_stream_continuation(&self) -> bool {
        self.body.is_stream_continuation()
    }
}
