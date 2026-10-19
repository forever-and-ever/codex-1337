use super::HistoryCell;
use super::PrefixedWrappedHistoryCell;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

pub(crate) fn marker() -> Span<'static> {
    Span::styled("• ", style())
}

fn style() -> Style {
    Style::default().fg(Color::Yellow).bold()
}

pub(crate) fn title(mut line: Line<'static>) -> Line<'static> {
    line.style = line.style.patch(style());
    for span in &mut line.spans {
        span.style = span.style.patch(style());
    }
    line
}

#[derive(Debug, Clone)]
pub(crate) struct NotificationCell(PrefixedWrappedHistoryCell);

impl NotificationCell {
    pub(crate) fn new(message: Line<'static>) -> Self {
        Self(PrefixedWrappedHistoryCell::new(
            title(message),
            Line::from(marker()),
            "  ",
        ))
    }
}

impl HistoryCell for NotificationCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.0.display_lines(width)
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.0.display_hyperlink_lines(width)
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.0.transcript_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        crate::render::line_utils::prefix_lines(self.0.raw_lines(), marker(), "  ".into())
    }
}
