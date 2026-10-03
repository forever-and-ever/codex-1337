//! Live Activity transcript entries and bounded Goal / Activity / Reasoning display.

use super::ChatWidget;
use crate::history_cell::HistoryTimestamp;
use crate::history_cell::PrefixedWrappedHistoryCell;
use crate::history_cell::agent_prefix;
use crate::line_truncation::truncate_line_to_width;
use crate::render::renderable::Renderable;
use crate::ui_consts::LIVE_PREFIX_COLS;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;
use codex_protocol::protocol::WorkStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

impl ChatWidget {
    pub(super) fn on_work_status_updated(&mut self, status: WorkStatus) {
        if status.activity.trim().is_empty() {
            self.current_work_status = None;
            self.request_redraw();
            return;
        }
        if self
            .current_work_status
            .as_ref()
            .is_some_and(|current| current.activity == status.activity)
        {
            return;
        }
        let emitted_at = status
            .emitted_at
            .unwrap_or_else(|| chrono::Utc::now().timestamp());
        let timestamp = HistoryTimestamp::from_millis(emitted_at.saturating_mul(1000));
        self.add_to_history(PrefixedWrappedHistoryCell::new(
            Line::from(vec!["Activity ".bold(), status.activity.clone().into()]),
            Line::from(agent_prefix(timestamp, /*first*/ true)),
            Line::from(agent_prefix(timestamp, /*first*/ false)),
        ));
        self.current_work_status = Some(status);
        self.request_redraw();
    }
}

pub(super) struct WorkStatusView<'a> {
    pub goal: Option<&'a str>,
    pub status: Option<&'a WorkStatus>,
    pub reasoning: Option<&'a str>,
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
            )])
            .chain(self.reasoning.map(|text| ("Reasoning", Some(text))));
        let mut lines = Vec::new();
        for (label, value) in zones {
            // Goal text is user-controlled and unbounded; never lay out the full objective.
            let bounded: String = value.unwrap_or("—").chars().take(1024).collect();
            let value = bounded.split_whitespace().collect::<Vec<_>>().join(" ");
            let value: String = value.chars().filter(|ch| !ch.is_control()).collect();
            let line = Line::from(vec![format!("{label:<9} ").dim(), value.into()]);
            let mut wrapped = word_wrap_lines(
                [line],
                RtOptions::new(usize::from(width))
                    .initial_indent(Line::from(" ".repeat(usize::from(LIVE_PREFIX_COLS))))
                    .subsequent_indent(Line::from(" ".repeat(usize::from(
                        width.saturating_sub(1).min(10 + LIVE_PREFIX_COLS),
                    )))),
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
