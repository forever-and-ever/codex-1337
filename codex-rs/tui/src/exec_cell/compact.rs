//! Owned-transcript command previews with full command/output retained for local disclosure.

use super::model::ExecCell;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell::ActivityDisclosure;
use crate::history_cell::HistoryRenderMode;
use crate::history_cell::activity_preview::clipped_line;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::render::highlight::highlight_bash_to_lines;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::style::Stylize;

impl ExecCell {
    pub(super) fn command_disclosure(&self, width: u16) -> Option<ActivityDisclosure> {
        let [call] = self.group.calls.as_slice() else {
            return None;
        };
        // Interaction output is intentionally absent from the expanded transcript.
        if !call.is_unified_exec_interaction()
            && let Some(output) = &call.output
            && output.line_counts().1 > 0
        {
            return Some(ActivityDisclosure::Generic);
        }
        if !self
            .group
            .details
            .lines_after(/*after_calls*/ 1, width, HistoryRenderMode::Rich)
            .is_empty()
        {
            return Some(ActivityDisclosure::Generic);
        }
        let script = strip_bash_lc_and_escape(&call.command);
        if script.lines().nth(/*n*/ 1).is_some()
            || self
                .compact_command_lines(u16::MAX)
                .first()
                .is_some_and(|header| header.width() > usize::from(width))
        {
            return Some(ActivityDisclosure::Generic);
        }
        None
    }

    pub(super) fn compact_command_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let [call] = self.group.calls.as_slice() else {
            return Vec::new();
        };
        if call.is_unified_exec_interaction()
            && !self.is_active()
            && call.output.as_ref().is_some_and(|output| {
                output.exit_code == 0 && output.lines().all(|line| line.trim().is_empty())
            })
        {
            return Vec::new();
        }
        let failed = call.output.as_ref().filter(|output| output.exit_code != 0);
        let marker = if failed.is_some() {
            "•".red().bold()
        } else if self.is_active() {
            activity_indicator(
                call.start_time,
                MotionMode::from_animations_enabled(self.animations_enabled()),
                ReducedMotionIndicator::StaticBullet,
            )
            .unwrap_or_else(|| "•".dim())
        } else {
            "•".green().bold()
        };
        let title = if let Some(output) = failed {
            format!("Failed (exit {})", output.exit_code)
        } else if self.is_active() {
            "Running".to_owned()
        } else if call.is_unified_exec_interaction() {
            "Background output".to_owned()
        } else {
            "Ran".to_owned()
        };
        let script = strip_bash_lc_and_escape(&call.command);
        // Highlight before clipping so the compact row keeps the same shell token colors.
        let mut command_lines = highlight_bash_to_lines(&script).into_iter();
        let mut header = command_lines.next().unwrap_or_default();
        let mut heading = vec![marker, " ".into()];
        if !self.is_active() {
            heading.extend(
                self.completed_at
                    .map(crate::history_cell::HistoryTimestamp::span),
            );
        }
        heading.extend([title.bold(), " ".into()]);
        header.spans.splice(0..0, heading);
        if command_lines.next().is_some() {
            header.push_span(" …");
        }
        vec![clipped_line(header, width)]
    }
}
