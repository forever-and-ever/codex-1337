//! The rest row: the status row's place between turns. It stays up so that the composer never
//! jumps, says how the thread's last turn ended, and carries the engaged shimmer's idle paint.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use super::BottomPane;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::motion::MotionMode;
use crate::render::renderable::Renderable;

/// How the thread's last turn ended, as the rest row says it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Rest {
    /// No turn has ended since the thread came into view.
    #[default]
    Ready,
    Finished,
    Interrupted,
    Failed,
}

impl Rest {
    fn caption(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Finished => "Finished",
            Self::Interrupted => "Interrupted",
            Self::Failed => "Failed",
        }
    }
}

/// The rest row as a frame paints it.
pub(super) struct RestRow<'a>(&'a BottomPane);

impl BottomPane {
    /// Records how the turn that just ended ended.
    pub(crate) fn set_rest(&mut self, rest: Rest) {
        if self.rest != rest {
            self.rest = rest;
            self.request_redraw();
        }
    }

    /// The rest row, while no turn runs and nothing else holds the status row's place.
    pub(super) fn rest_row(&self) -> Option<RestRow<'_>> {
        (!self.is_task_running && self.status.is_none() && self.hook_status_message.is_none())
            .then_some(RestRow(self))
    }
}

impl Renderable for RestRow<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let pane = self.0;
        let caption = pane.rest.caption();
        let animated = pane.animations_enabled && pane.frame_requester.mode().glints();
        let mark = MotionMode::from_animations_enabled(animated && pane.effects.progress);
        let letters = MotionMode::from_animations_enabled(animated && pane.effects.shimmer);
        let mut spans: Vec<Span<'static>> = vec!["• ".dim(), caption.dim()];
        if crate::shimmers::dress_status(&mut spans, caption, mark, letters) {
            // The idle paint moves on every beat the row glints.
            pane.frame_requester.schedule_glint_in(Duration::ZERO);
        }
        if let Some(summary) = pane.unified_exec_footer.summary_text() {
            spans.push(" · ".dim());
            spans.push(summary.dim());
        }
        let line =
            truncate_line_with_ellipsis_if_overflow(Line::from(spans), usize::from(area.width));
        crate::tui::glint::paint_status(area, /*height*/ 1, buf, |buf| {
            Paragraph::new(line).render(area, buf);
            crate::shimmers::shade_status(area, buf);
        });
    }

    fn desired_height(&self, _width: u16) -> u16 {
        1
    }
}
