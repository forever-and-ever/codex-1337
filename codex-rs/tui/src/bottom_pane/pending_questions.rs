//! A question stays visible without taking the ordinary composer's focus.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::render::renderable::Renderable;

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
        let mut lines = crate::wrapping::word_wrap_lines([header], usize::from(width.max(1)));
        if let Some(first) = &self.first {
            lines.extend(
                textwrap::wrap(first, usize::from(width.max(1)))
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
