//! Bounded Goal / Activity display between the entry box and its footer.

use crate::line_truncation::truncate_line_to_width;
use crate::render::renderable::Renderable;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;
use codex_protocol::protocol::WorkStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

pub(super) struct WorkStatusView<'a> {
    pub goal: Option<&'a str>,
    pub status: Option<&'a WorkStatus>,
}

impl WorkStatusView<'_> {
    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let zones = self
            .goal
            .map(|goal| ("Goal", Some(goal)))
            .into_iter()
            .chain([(
                "Activity",
                self.status.map(|status| status.activity.as_str()),
            )]);
        let rule = Line::from("─".repeat(usize::from(width)).dim());
        let mut lines = vec![rule.clone()];
        for (label, value) in zones {
            // Goal text is user-controlled and unbounded; never lay out the full objective.
            let bounded: String = value.unwrap_or("—").chars().take(1024).collect();
            let value = bounded.split_whitespace().collect::<Vec<_>>().join(" ");
            let value: String = value.chars().filter(|ch| !ch.is_control()).collect();
            let line = Line::from(vec![format!("{label:<8} ").dim(), value.into()]);
            let mut wrapped = word_wrap_lines(
                [line],
                RtOptions::new(usize::from(width)).subsequent_indent(Line::from(
                    " ".repeat(usize::from(width.saturating_sub(1).min(9))),
                )),
            );
            if wrapped.len() > 2 {
                wrapped.truncate(2);
                let last = &mut wrapped[1];
                *last = truncate_line_to_width(last.clone(), usize::from(width - 1));
                last.spans.push("…".dim());
            }
            lines.extend(
                wrapped
                    .into_iter()
                    .map(|line| truncate_line_to_width(line, usize::from(width))),
            );
        }
        lines.push(rule);
        lines
    }
}

impl Renderable for WorkStatusView<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.lines(area.width)).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.lines(width).len() as u16
    }
}
