use crate::history_cell::HistoryCell;
use ratatui::text::Line;

/// Already-painted rows: selection must not add transcript gutters or provenance styling.
#[derive(Debug)]
pub(crate) struct RenderedSurface(pub(crate) Vec<Line<'static>>);

impl HistoryCell for RenderedSurface {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.0.clone()
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.0
            .iter()
            .map(|line| Line::from(line.to_string()))
            .collect()
    }
}
