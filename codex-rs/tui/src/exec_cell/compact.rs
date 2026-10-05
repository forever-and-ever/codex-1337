//! Owned-transcript command previews with full command/output retained for local disclosure.

use super::model::ExecCell;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell::ActivityDisclosure;
use crate::history_cell::activity_preview::clipped_line;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::render::highlight::highlight_bash_to_lines;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::style::Stylize;
use ratatui::text::Line;

impl ExecCell {
    pub(crate) fn compact_header_hidden(&self) -> bool {
        let [call] = self.group.calls.as_slice() else {
            return false;
        };
        call.is_unified_exec_interaction()
            && !self.is_active()
            && call.output.as_ref().is_some_and(|output| {
                output.exit_code == 0 && output.lines().all(|line| line.trim().is_empty())
            })
    }

    pub(super) fn command_disclosure(&self, width: u16) -> Option<ActivityDisclosure> {
        // Interaction output is intentionally absent from the expanded transcript.
        let has_output = self
            .group
            .details
            .returned_output_has_details()
            .unwrap_or_else(|| {
                self.group.calls.iter().any(|call| {
                    !call.is_unified_exec_interaction()
                        && call.output.as_ref().is_some_and(|output| {
                            output.lines().any(|line| !line.trim().is_empty())
                        })
                })
            });
        (has_output || crate::history_cell::HistoryCell::activity_arguments_truncated(self, width))
            .then_some(ActivityDisclosure::Generic)
    }

    pub(super) fn compact_command_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let [call] = self.group.calls.as_slice() else {
            return Vec::new();
        };
        if self.compact_header_hidden() {
            return Vec::new();
        }
        let script = strip_bash_lc_and_escape(&call.command);
        // Highlight before clipping so the compact row keeps the same shell token colors.
        let mut command_lines = highlight_bash_to_lines(&script).into_iter();
        let mut header = command_lines.next().unwrap_or_default();
        header.spans.splice(0..0, self.command_heading().0.spans);
        if command_lines.next().is_some() {
            header.push_span(" …");
        }
        vec![clipped_line(header, width)]
    }

    pub(super) fn command_heading(
        &self,
    ) -> (
        Line<'static>,
        Option<crate::history_cell::activity_parts::ActivityHeader>,
    ) {
        let [call] = self.group.calls.as_slice() else {
            return (Line::default(), None);
        };
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
        let (verb, suffix) = if let Some(output) = failed {
            ("Failed", format!(" (exit {})", output.exit_code))
        } else if self.is_active() {
            ("Running", String::new())
        } else if call.is_unified_exec_interaction() {
            ("Background", " output".into())
        } else {
            ("Ran", String::new())
        };
        let heading = vec![marker, " ".into()];
        let mut heading = Line::from(heading);
        heading.push_span(verb.bold());
        let header = crate::history_cell::activity_parts::ActivityHeader::at_end(&heading);
        heading.extend([suffix.bold(), " ".into()]);
        (heading, Some(header))
    }
}
