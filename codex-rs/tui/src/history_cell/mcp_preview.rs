//! Single-line invocation previews; complete arguments and results remain in the transcript.

use super::McpToolCallCell;
use super::result::McpResultKind;
use crate::history_cell::activity_preview::ArgumentSyntax;
use crate::history_cell::activity_preview::compact_arguments;
use crate::history_cell::activity_preview::single_line;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::style::accent_color;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::style::Stylize;
use ratatui::text::Line;

impl McpToolCallCell {
    pub(super) fn compact_tool_name(&self) -> String {
        let mut label = self
            .invocation
            .arguments
            .as_ref()
            .filter(|_| self.result_kind() == McpResultKind::NodeRepl)
            .and_then(|arguments| arguments.get("title"))
            .and_then(serde_json::Value::as_str)
            .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| format!("{}.{}", self.invocation.server, self.invocation.tool));
        if let Some(environment_id) = &self.invocation.environment_id {
            label.push_str(&format!(" (environment {environment_id})"));
        }
        label
    }

    pub(super) fn compact_mcp_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let status = self.success();
        let marker = match status {
            Some(true) => "•".green().bold(),
            Some(false) => "•".red().bold(),
            None => activity_indicator(
                Some(self.start_time),
                MotionMode::from_animations_enabled(self.animations_enabled),
                ReducedMotionIndicator::StaticBullet,
            )
            .unwrap_or_else(|| "•".dim()),
        };
        let mut spans = vec![marker, " ".into()];
        spans.extend([
            self.outcome_verb().bold(),
            " ".into(),
            single_line(&self.invocation.server).fg(accent_color()),
            ".".into(),
            single_line(&self.invocation.tool).fg(accent_color()),
            "(".into(),
            self.invocation
                .arguments
                .as_ref()
                .map(|arguments| compact_arguments(arguments, ArgumentSyntax::Keywords))
                .unwrap_or_default()
                .dim(),
            ")".into(),
        ]);
        if let Some(environment_id) = &self.invocation.environment_id {
            spans.push(format!(" (environment {})", single_line(environment_id)).dim());
        }
        vec![super::mcp_header_line(
            crate::history_cell::activity_preview::compact_header(
                Line::from(spans),
                usize::from(width),
            ),
        )]
    }
}
