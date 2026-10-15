//! Live Activity transcript entries, Working labels, and bounded Goal / Reasoning display.

use super::ChatWidget;
use crate::history_cell::HistoryTimestamp;
use crate::history_cell::WorkStatusCell;
use crate::line_truncation::truncate_line_to_width;
use crate::render::renderable::Renderable;
use crate::status_indicator_widget::StatusDetailsCapitalization;
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
use unicode_segmentation::UnicodeSegmentation;

impl ChatWidget {
    pub(super) fn on_work_status_updated(&mut self, status: WorkStatus) {
        if status.activity.trim().is_empty() {
            self.clear_work_status();
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
        self.add_to_history(WorkStatusCell::new(status.activity.clone(), timestamp));
        self.current_work_status = Some(status);
        self.refresh_work_status_header();
        self.request_redraw();
    }

    pub(super) fn clear_work_status(&mut self) {
        if self.current_work_status.take().is_some() {
            self.refresh_work_status_header();
        }
    }

    pub(super) fn work_status_header(&self, header: &str) -> String {
        if header == "Working"
            && let Some(status) = &self.current_work_status
        {
            return activity_header(&status.activity);
        }
        header.to_owned()
    }

    fn refresh_work_status_header(&mut self) {
        let status = &self.status_state.current_status;
        if status.header == "Working" {
            let header = self.work_status_header(&status.header);
            self.bottom_pane.update_status(
                header,
                status.details.clone(),
                StatusDetailsCapitalization::Preserve,
                status.details_max_lines,
            );
        }
    }
}

fn activity_header(activity: &str) -> String {
    let activity = activity.split_whitespace().collect::<Vec<_>>().join(" ");
    let activity: String = activity.chars().filter(|ch| !ch.is_control()).collect();
    if activity.is_empty() {
        return "Working".into();
    }
    if activity.chars().count() <= WorkStatus::MAX_ACTIVITY_CHARS {
        return activity;
    }
    // Legacy servers may exceed the current limit. Keep whole graphemes and count the ellipsis.
    let mut remaining = WorkStatus::MAX_ACTIVITY_CHARS - 1;
    let mut header: String = activity
        .graphemes(true)
        .take_while(|grapheme| {
            let len = grapheme.chars().count();
            if len > remaining {
                return false;
            }
            remaining -= len;
            true
        })
        .collect();
    header.push('…');
    header
}

pub(super) struct WorkStatusView<'a> {
    pub goal: Option<&'a str>,
    pub reasoning: Option<&'a str>,
}

impl WorkStatusView<'_> {
    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let zones = self
            .goal
            .map(|goal| ("Goal", goal))
            .into_iter()
            .chain(self.reasoning.map(|text| ("Reasoning", text)));
        let mut lines = Vec::new();
        for (label, value) in zones {
            // Goal text is user-controlled and unbounded; never lay out the full objective.
            let bounded: String = value.chars().take(1024).collect();
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
