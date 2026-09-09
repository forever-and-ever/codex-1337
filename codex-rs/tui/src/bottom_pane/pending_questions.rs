//! A question stays visible without taking the ordinary composer's focus.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::render::renderable::Renderable;
use crate::ui_consts::LIVE_PREFIX_COLS;
use crate::wrapping::RtOptions;

#[derive(Default)]
pub(super) struct PendingQuestions {
    pub(super) count: usize,
    pub(super) first: Option<String>,
}

impl PendingQuestions {
    fn lines(&self, width: u16) -> Vec<Line<'_>> {
        if self.count == 0 {
            return Vec::new();
        }
        let header = Line::from(vec![
            format!(
                "{} pending question{}",
                self.count,
                if self.count == 1 { "" } else { "s" }
            )
            .bold(),
            " · ".dim(),
            "/questions".cyan(),
            " to answer or dismiss".dim(),
        ]);
        let indent = " ".repeat(usize::from(LIVE_PREFIX_COLS));
        let mut lines = crate::wrapping::word_wrap_lines(
            [header],
            RtOptions::new(usize::from(width.max(1)))
                .initial_indent(Line::from(indent.clone()))
                .subsequent_indent(Line::from(indent.clone())),
        );
        if let Some(first) = &self.first {
            lines.extend(
                textwrap::wrap(
                    first,
                    textwrap::Options::new(usize::from(width.max(1)))
                        .initial_indent(&indent)
                        .subsequent_indent(&indent),
                )
                .into_iter()
                .take(2)
                .map(|s| Line::from(s.into_owned())),
            );
        }
        lines
    }
}

impl Renderable for PendingQuestions {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        use ratatui::widgets::Widget;
        Paragraph::new(self.lines(area.width)).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.lines(width).len() as u16
    }
}
