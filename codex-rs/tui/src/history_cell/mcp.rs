//! MCP tool-call, inventory, and output history cells.
//! Tool output previews share a three-row budget across all result blocks;
//! the expanded transcript retains the full text.
//! Invocation and result rows retain exact logical source ranges through wrapping and gutters.
//! Environment ownership is rendered separately from raw server names used by special handlers.

use super::*;
use crate::history_cell::activity_parts::FoldState;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;

use codex_protocol::mcp::is_node_repl_backed_server;

#[path = "mcp_result.rs"]
mod result;

#[path = "mcp_outcome.rs"]
mod outcome;

#[path = "mcp_preview.rs"]
mod preview;

#[path = "mcp_file_changes.rs"]
mod file_changes;
use file_changes::FileChangeDetail;

#[path = "computer_activity.rs"]
mod computer_activity;
pub(crate) use computer_activity::ComputerActivityCell;

use crate::style::StatusTone;
use crate::style::accent_color;
use crate::style::accent_style;
use crate::style::status_style;
use crate::terminal_hyperlinks::LogicalLineSource;
use crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;
use crate::terminal_hyperlinks::remap_source_wrapped_line;
use crate::text_formatting::format_json_compact;
use crate::tool_output::ToolOutputPreview;
use codex_app_server_protocol::McpServerConnectionStatus;
use result::McpContentBlock;
use result::McpResultKind;
use result::McpToolResult;
use std::borrow::Cow;

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

fn mcp_auth_status_label(status: McpAuthStatus) -> &'static str {
    match status {
        McpAuthStatus::Unknown => "Unknown",
        McpAuthStatus::Unsupported => "Unsupported",
        McpAuthStatus::NotLoggedIn => "Not logged in",
        McpAuthStatus::BearerToken => "Bearer token",
        McpAuthStatus::OAuth => "OAuth",
    }
}
#[derive(Clone, Debug)]
pub(crate) struct McpToolCallCell {
    pub(crate) completed_at: Option<HistoryTimestamp>,
    call_id: String,
    invocation: McpInvocation,
    start_time: Instant,
    duration: Option<Duration>,
    result: Option<Result<McpToolResult, String>>,
    animations_enabled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct McpInvocation {
    pub(crate) server: String,
    pub(crate) environment_id: Option<String>,
    pub(crate) tool: String,
    pub(crate) arguments: Option<serde_json::Value>,
}

impl McpInvocation {
    pub(crate) fn is_computer_activity(&self) -> bool {
        self.server == "cua_repl"
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum McpToolCallRenderMode {
    Summary,
    /// Compact presentation used in normal conversation history.
    Display,
    /// Complete invocation and result used by the Ctrl+T transcript.
    Transcript,
    Arguments,
    NestedArguments,
    Expanded,
}

#[derive(serde::Deserialize)]
struct NodeReplExecOutput {
    exit_code: i64,
    output: String,
}

impl McpToolCallCell {
    pub(super) fn receipt_parts(
        &self,
        width: u16,
        expanded: &FoldState,
        full: bool,
        heading: bool,
    ) -> super::activity_parts::ActivityParts {
        let Some(Ok(result)) = &self.result else {
            return Default::default();
        };
        let Some(receipt) = &result.file_changes else {
            return Default::default();
        };
        let width = width.saturating_sub(crate::transcript_geometry::MARKER_COLUMNS);
        let (mut lines, mut controls) = receipt.lines(
            width,
            FileChangeDetail::Disclosed,
            &self.call_id,
            expanded,
            heading,
        );
        if full && !controls.is_empty() {
            let expanded = expanded
                .iter()
                .cloned()
                .chain(controls.iter().map(|control| control.id.clone()))
                .collect();
            (lines, controls) = receipt.lines(
                width,
                FileChangeDetail::Disclosed,
                &self.call_id,
                &expanded,
                heading,
            );
        }
        super::activity_parts::ActivityParts {
            lines: prefix_hyperlink_lines(
                lines,
                if heading { "• ".dim() } else { "  ".into() },
                "  ".into(),
            ),
            controls,
            ..Default::default()
        }
    }

    pub(super) fn edit_parts(
        &self,
        width: u16,
        expanded: &FoldState,
        full: bool,
    ) -> Option<super::invocation::EditProjection> {
        let receipt = self.result.as_ref()?.as_ref().ok()?.file_changes.as_ref()?;
        let applied = receipt.is_applied();
        let parts = self.receipt_parts(width, expanded, full, !applied);
        Some(if applied {
            super::invocation::EditProjection::Applied(parts)
        } else {
            super::invocation::EditProjection::Other(parts)
        })
    }

    pub(super) fn stored_output_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.render_detail_lines(width, McpToolCallRenderMode::Expanded)
    }
    pub(super) fn invocation_input(&self, width: u16) -> super::invocation::InvocationInput {
        super::invocation::InvocationInput {
            parts: super::activity_parts::ActivityParts {
                lines: self.render_hyperlink_lines(width, McpToolCallRenderMode::Summary),
                ..Default::default()
            },
            // Summary reserves its last column for ellipsis; width 1 drops the marker.
            indicators: (self.success().is_none() && width > 1)
                .then(|| super::invocation::ActivityIndicator {
                    line: 0,
                    start: self.start_time,
                    mode: MotionMode::from_animations_enabled(self.animations_enabled),
                })
                .into_iter()
                .collect(),
        }
    }
    fn nested_receipt_parts(
        &self,
        width: u16,
        expanded: &FoldState,
        lines: Vec<HyperlinkLine>,
    ) -> super::activity_parts::ActivityParts {
        let mut receipt = self.receipt_parts(
            width.saturating_sub(crate::transcript_geometry::CHILD_INDENT),
            expanded,
            false,
            true,
        );
        for control in &mut receipt.controls {
            control.line += lines.len();
        }
        let mut parts = super::activity_parts::ActivityParts {
            lines,
            header: self.activity_header(),
            output: None,
            controls: receipt.controls,
        };
        parts.lines.extend(prefix_hyperlink_lines(
            receipt.lines,
            crate::transcript_geometry::CHILD_PREFIX.into(),
            crate::transcript_geometry::CHILD_PREFIX.into(),
        ));
        parts
    }
    fn file_change_lines(&self, width: u16, detail: FileChangeDetail) -> Vec<HyperlinkLine> {
        let Some(Ok(result)) = &self.result else {
            return Vec::new();
        };
        let Some(receipt) = &result.file_changes else {
            return Vec::new();
        };
        let (lines, _) = receipt.lines(
            width.saturating_sub(crate::transcript_geometry::CHILD_INDENT),
            detail,
            &self.call_id,
            &Default::default(),
            true,
        );
        prefix_hyperlink_lines(
            lines,
            crate::transcript_geometry::CHILD_PREFIX.into(),
            crate::transcript_geometry::CHILD_PREFIX.into(),
        )
    }
    fn header_verb(&self) -> &'static str {
        self.outcome_verb()
    }
    pub(crate) fn new(
        call_id: String,
        invocation: McpInvocation,
        animations_enabled: bool,
    ) -> Self {
        Self {
            call_id,
            completed_at: None,
            invocation,
            start_time: Instant::now(),
            duration: None,
            result: None,
            animations_enabled,
        }
    }

    pub(crate) fn call_id(&self) -> &str {
        &self.call_id
    }

    pub(crate) fn is_active(&self) -> bool {
        self.result.is_none()
    }

    pub(crate) fn complete(
        &mut self,
        duration: Duration,
        result: Result<codex_protocol::mcp::CallToolResult, String>,
    ) {
        let result = result.map(|result| McpToolResult::new(result, self.result_kind()));
        self.duration = Some(duration);
        self.result = Some(result);
    }

    pub(crate) fn success(&self) -> Option<bool> {
        match self.result.as_ref() {
            Some(Ok(result)) => Some(!result.is_error),
            Some(Err(_)) => Some(false),
            None => None,
        }
    }

    pub(crate) fn mark_failed(&mut self) {
        let elapsed = self.start_time.elapsed();
        self.duration = Some(elapsed);
        self.result = Some(Err("interrupted".to_string()));
    }

    pub(crate) fn freeze_snapshot(&mut self) {
        self.animations_enabled = false;
    }

    fn result_kind(&self) -> McpResultKind {
        if self.invocation.is_computer_activity()
            || (is_node_repl_backed_server(&self.invocation.server) && self.invocation.tool == "js")
        {
            McpResultKind::NodeRepl
        } else {
            McpResultKind::Standard
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "The MCP marker retains its established distinct foreground within the fork invocation header."
    )]
    fn render_hyperlink_lines(
        &self,
        width: u16,
        mode: McpToolCallRenderMode,
    ) -> Vec<HyperlinkLine> {
        let mut lines: Vec<HyperlinkLine> = Vec::new();
        let status = self.success();
        let node_repl = self.result_kind() == McpResultKind::NodeRepl;
        let compact = node_repl
            && matches!(
                mode,
                McpToolCallRenderMode::Display | McpToolCallRenderMode::Summary
            );
        let bullet = match status {
            Some(true) => "•".green().bold(),
            Some(false) => "•".red().bold(),
            None => activity_indicator(
                Some(self.start_time),
                MotionMode::from_animations_enabled(self.animations_enabled),
                ReducedMotionIndicator::StaticBullet,
            )
            .unwrap_or_else(|| "•".dim()),
        };
        let nested = matches!(
            mode,
            McpToolCallRenderMode::Summary | McpToolCallRenderMode::NestedArguments
        );
        let header_text = self.header_verb();

        let mut invocation_line = if compact {
            Line::from(self.compact_tool_name().fg(accent_color()))
        } else {
            line_to_static(&format_mcp_invocation(&self.invocation))
        };
        invocation_line.spans.insert(0, "MCP ".white().bold());
        let mut compact_spans = vec![bullet.clone(), " ".into()];
        if !nested || status == Some(false) {
            let verb = if status == Some(false) {
                header_text.red().bold()
            } else {
                header_text.bold()
            };
            compact_spans.extend([verb, " ".into()]);
        }
        let mut compact_header = Line::from(compact_spans.clone());
        let reserved = compact_header.width();

        let inline_invocation = mode == McpToolCallRenderMode::NestedArguments
            || (!matches!(
                mode,
                McpToolCallRenderMode::Arguments
                    | McpToolCallRenderMode::NestedArguments
                    | McpToolCallRenderMode::Expanded
            ) && (mode == McpToolCallRenderMode::Summary
                || compact
                || invocation_line.width() <= (width as usize).saturating_sub(reserved)));

        if inline_invocation {
            compact_header.extend(invocation_line.spans.clone());
            lines.push(mcp_header_line(
                if compact || mode == McpToolCallRenderMode::Summary {
                    truncate_line_with_ellipsis_if_overflow(compact_header, width as usize)
                } else {
                    compact_header
                },
            ));
        } else {
            compact_spans.pop(); // drop trailing space for standalone header
            lines.push(mcp_header_line(Line::from(compact_spans)));

            let opts = RtOptions::new(
                (width as usize)
                    .saturating_sub(usize::from(crate::transcript_geometry::CHILD_INDENT))
                    .max(/*other*/ 1),
            );
            let mut invocation = vec![HyperlinkLine::new(invocation_line)];
            if matches!(
                mode,
                McpToolCallRenderMode::Arguments
                    | McpToolCallRenderMode::NestedArguments
                    | McpToolCallRenderMode::Expanded
            ) {
                super::output_provenance::mark_lines(
                    &mut invocation,
                    super::output_provenance::OutputProvenance::Formatted,
                );
            }
            super::output_provenance::hang(&mut invocation, crate::warpaint::Bracket::Call);
            let body_lines = adaptive_wrap_hyperlink_lines(&invocation, opts);
            lines.extend(prefix_hyperlink_lines(
                body_lines,
                crate::transcript_geometry::CHILD_BRANCH.dim(),
                crate::transcript_geometry::CHILD_PREFIX.into(),
            ));
        }

        if mode == McpToolCallRenderMode::NestedArguments {
            super::output_provenance::mark_lines(
                &mut lines,
                super::output_provenance::OutputProvenance::Client,
            );
        }
        let detail_lines = if matches!(
            mode,
            McpToolCallRenderMode::Arguments
                | McpToolCallRenderMode::NestedArguments
                | McpToolCallRenderMode::Summary
        ) {
            Vec::new()
        } else {
            self.render_detail_lines(width, mode)
        };
        if !detail_lines.is_empty() {
            let initial_prefix = if inline_invocation {
                crate::transcript_geometry::CHILD_BRANCH.dim()
            } else {
                crate::transcript_geometry::CHILD_PREFIX.into()
            };
            lines.extend(prefix_hyperlink_lines(
                detail_lines,
                initial_prefix,
                crate::transcript_geometry::CHILD_PREFIX.into(),
            ));
        }
        lines
    }

    /// Share one bounded preview across result blocks while retaining full transcript sources.
    fn render_detail_lines(&self, width: u16, mode: McpToolCallRenderMode) -> Vec<HyperlinkLine> {
        let detail_width = usize::from(width)
            .saturating_sub(usize::from(crate::transcript_geometry::CHILD_INDENT))
            .max(/*other*/ 1);
        let options = RtOptions::new(detail_width);
        let mut lines = Vec::new();
        if self.result.is_some() && mode == McpToolCallRenderMode::Expanded {
            lines.extend(adaptive_wrap_hyperlink_lines(
                &[super::output_provenance::client_notice(
                    "Stored result · exact capture unavailable",
                )],
                options.clone(),
            ));
        }
        let mut preview = ToolOutputPreview::new(detail_width, /*omitted*/ 0);
        match &self.result {
            Some(Ok(result)) => {
                for block in &result.content {
                    // Image blocks can carry text as well as their image indication.
                    if self.result_kind() == McpResultKind::NodeRepl
                        && block.is_image
                        && block.text().is_some()
                    {
                        let marker = HyperlinkLine::new("Returned image".dim().into());
                        if mode == McpToolCallRenderMode::Display {
                            preview.push_hyperlink_line(marker);
                        } else {
                            lines.extend(remap_source_wrapped_line(
                                &marker,
                                crate::wrapping::word_wrap_line_with_source(
                                    &marker.line,
                                    detail_width,
                                ),
                            ));
                        }
                    }
                    let Some(text) = self.result_block_text(block, mode) else {
                        continue;
                    };
                    for segment in text.lines() {
                        if mode == McpToolCallRenderMode::Display {
                            preview.push_line(Line::from(segment.dim()));
                        } else {
                            let line = Line::from(segment.to_owned().dim());
                            lines.extend(adaptive_wrap_hyperlink_lines(
                                &[HyperlinkLine::new(line)],
                                options.clone(),
                            ));
                        }
                    }
                }
            }
            Some(Err(error)) => {
                let text = format!("Error: {error}");
                for segment in text.lines() {
                    if mode == McpToolCallRenderMode::Display {
                        preview.push_line(Line::from(segment.dim()));
                    } else {
                        let line = Line::from(segment.to_owned().dim());
                        lines.extend(adaptive_wrap_hyperlink_lines(
                            &[HyperlinkLine::new(line)],
                            options.clone(),
                        ));
                    }
                }
            }
            None => {}
        }
        if mode == McpToolCallRenderMode::Display {
            preview.finish_hyperlink_lines()
        } else {
            super::output_provenance::mark_lines(
                &mut lines,
                super::output_provenance::OutputProvenance::Client,
            );
            lines
        }
    }

    /// Keep compact code-mode summaries while retaining complete transcript code and output.
    fn result_block_text<'a>(
        &self,
        block: &'a McpContentBlock,
        mode: McpToolCallRenderMode,
    ) -> Option<Cow<'a, str>> {
        let node_repl = self.result_kind() == McpResultKind::NodeRepl;
        if mode == McpToolCallRenderMode::Expanded {
            return if node_repl {
                block
                    .text()
                    .map(Cow::Borrowed)
                    .or_else(|| block.render_detail())
            } else {
                block.render_detail()
            };
        }
        if mode != McpToolCallRenderMode::Display {
            return Some(Cow::Borrowed(block.render_full()));
        }
        if node_repl && self.success() == Some(true) {
            let meaningful_output = block.text().and_then(|text| {
                if text.starts_with("Script completed\n") {
                    return text
                        .split_once("\nOutput:\n")
                        .map(|(_, output)| Cow::Borrowed(output));
                }
                serde_json::from_str::<NodeReplExecOutput>(text)
                    .ok()
                    .filter(|output| output.exit_code == 0)
                    .map(|output| Cow::Owned(output.output))
            });
            match meaningful_output {
                Some(output) if output.is_empty() => None,
                Some(output) => Some(
                    format_json_compact(&output)
                        .map(Cow::Owned)
                        .unwrap_or(output),
                ),
                None => Some(block.text().map_or_else(
                    || block.render(),
                    |text| {
                        format_json_compact(text)
                            .map(Cow::Owned)
                            .unwrap_or(Cow::Borrowed(text))
                    },
                )),
            }
        } else if node_repl && let Some(output) = block.text() {
            Some(
                format_json_compact(output)
                    .map(Cow::Owned)
                    .unwrap_or(Cow::Borrowed(output)),
            )
        } else {
            Some(block.render())
        }
    }
}

impl HistoryCell for McpToolCallCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.result.as_ref().and(self.completed_at)
    }
    fn outer_call_id(&self) -> Option<&str> {
        Some(&self.call_id)
    }
    fn tool_call_ids(&self) -> Vec<&str> {
        vec![self.call_id()]
    }

    fn nested_summary_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = self.render_hyperlink_lines(width, McpToolCallRenderMode::Summary);
        lines.extend(self.file_change_lines(width, FileChangeDetail::Summary));
        lines
    }

    fn nested_detail_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut nested = self.clone();
        nested.completed_at = None;
        let mut lines =
            nested.render_hyperlink_lines(width, McpToolCallRenderMode::NestedArguments);
        lines.extend(self.file_change_lines(width, FileChangeDetail::Full));
        lines
    }

    fn nested_summary_parts(
        &self,
        width: u16,
        expanded: &FoldState,
    ) -> super::activity_parts::ActivityParts {
        let lines = self.render_hyperlink_lines(width, McpToolCallRenderMode::Summary);
        self.nested_receipt_parts(width, expanded, lines)
    }

    fn nested_activity_parts(
        &self,
        width: u16,
        expanded: &FoldState,
    ) -> Option<super::activity_parts::ActivityParts> {
        let mut nested = self.clone();
        nested.completed_at = None;
        Some(nested.nested_receipt_parts(
            width,
            expanded,
            nested.render_hyperlink_lines(width, McpToolCallRenderMode::NestedArguments),
        ))
    }

    fn has_nested_details(&self, width: u16) -> bool {
        self.result
            .as_ref()
            .is_some_and(|result| matches!(result, Ok(result) if result.file_changes.is_some()))
            || self.activity_disclosure(width).is_some()
            || self.activity_arguments_truncated(width)
    }

    fn activity_parts(
        &self,
        width: u16,
        header_reserve: usize,
        expanded: &FoldState,
    ) -> Option<super::activity_parts::ActivityParts> {
        let Some(Ok(result)) = &self.result else {
            return None;
        };
        let receipt = result.file_changes.as_ref()?;
        let output_expanded = self.activity_ids().iter().any(|id| expanded.contains(id));
        let mut lines = if output_expanded {
            self.render_hyperlink_lines(width, McpToolCallRenderMode::Expanded)
        } else {
            self.compact_mcp_lines(
                width.saturating_sub(u16::try_from(header_reserve).unwrap_or(u16::MAX)),
            )
        };
        let offset = lines.len();
        let (file_lines, mut controls) = receipt.lines(
            width.saturating_sub(crate::transcript_geometry::CHILD_INDENT),
            FileChangeDetail::Disclosed,
            &self.call_id,
            expanded,
            true,
        );
        for control in &mut controls {
            control.line += offset;
        }
        lines.extend(prefix_hyperlink_lines(
            file_lines,
            crate::transcript_geometry::CHILD_PREFIX.into(),
            crate::transcript_geometry::CHILD_PREFIX.into(),
        ));
        Some(super::activity_parts::ActivityParts {
            lines,
            header: self.activity_header(),
            output: self.activity_disclosure(width),
            controls,
        })
    }
    fn activity_header(&self) -> Option<super::activity_parts::ActivityHeader> {
        let mut heading = Line::default();
        heading.push_span(self.header_verb());
        Some(super::activity_parts::ActivityHeader::at_end(&heading))
    }
    fn activity_disclosure(&self, width: u16) -> Option<ActivityDisclosure> {
        let has_output = match &self.result {
            Some(Ok(result)) => result.content.iter().any(|block| {
                self.result_block_text(block, McpToolCallRenderMode::Expanded)
                    .is_some_and(|text| !text.trim().is_empty())
                    || block.is_image
            }),
            Some(Err(error)) => !error.trim().is_empty(),
            None => false,
        };
        (has_output || self.activity_arguments_truncated(width))
            .then_some(ActivityDisclosure::Generic)
    }

    fn activity_arguments_truncated(&self, width: u16) -> bool {
        self.compact_mcp_lines(u16::MAX)
            .iter()
            .any(|line| line.width() > usize::from(width))
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![format!("mcp:{}", self.call_id)]
    }

    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = self.compact_mcp_lines(width);
        lines.extend(self.file_change_lines(width, FileChangeDetail::Summary));
        lines
    }

    fn activity_argument_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.render_hyperlink_lines(width, McpToolCallRenderMode::Arguments)
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = self.render_hyperlink_lines(width, McpToolCallRenderMode::Expanded);
        lines.extend(self.file_change_lines(width, FileChangeDetail::Full));
        lines
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.transcript_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = self.render_hyperlink_lines(width, McpToolCallRenderMode::Display);
        lines.extend(self.file_change_lines(width, FileChangeDetail::Summary));
        lines
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = self.render_hyperlink_lines(width, McpToolCallRenderMode::Transcript);
        lines.extend(self.file_change_lines(width, FileChangeDetail::Full));
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let header_text = self.header_verb();
        let mut lines = vec![Line::from(format!(
            "{header_text} {}",
            format_mcp_invocation(&self.invocation)
        ))];

        if let Some(result) = &self.result {
            match result {
                Ok(McpToolResult { content, .. }) => {
                    for block in content {
                        let text = block.render_full();
                        lines.extend(raw_lines_from_source(text));
                    }
                }
                Err(err) => lines.push(Line::from(format!("Error: {err}"))),
            }
        }

        lines.extend(visible_lines(
            self.file_change_lines(u16::MAX, FileChangeDetail::Full),
        ));
        lines
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        if !self.animations_enabled || self.result.is_some() {
            return None;
        }
        Some((self.start_time.elapsed().as_millis() / 50) as u64)
    }
}

/// The first two spans are the status bullet and its space, not invocation source text.
fn mcp_header_line(line: Line<'static>) -> HyperlinkLine {
    let mut source = LogicalLineSource::from_line(&Line::from(
        line.spans.iter().skip(/*n*/ 2).cloned().collect::<Vec<_>>(),
    ));
    source.prefix_bytes = line
        .spans
        .iter()
        .take(/*n*/ 2)
        .map(|span| span.content.len())
        .sum();
    let mut line = HyperlinkLine::new(line);
    line.source = Some(source);
    line
}

pub(crate) fn new_active_mcp_tool_call(
    call_id: String,
    invocation: McpInvocation,
    animations_enabled: bool,
) -> McpToolCallCell {
    McpToolCallCell::new(call_id, invocation, animations_enabled)
}
/// Render a summary of configured MCP servers from the current `Config`.
pub(crate) fn empty_mcp_output() -> WebHyperlinkHistoryCell {
    let mut docs_line = HyperlinkLine::new(Line::from("    See the "));
    docs_line.push_span(
        "MCP docs".underlined(),
        Some("https://developers.openai.com/codex/mcp"),
    );
    docs_line.push_span(" to configure them.".into(), /*destination*/ None);

    let lines = vec![
        HyperlinkLine::new("/mcp".magenta().into()),
        HyperlinkLine::from(""),
        HyperlinkLine::new(vec!["🔌  ".into(), "MCP Tools".bold()].into()),
        HyperlinkLine::from(""),
        HyperlinkLine::new("  • No MCP servers configured.".italic().into()),
        docs_line.style(Style::default().add_modifier(Modifier::DIM)),
    ];

    WebHyperlinkHistoryCell::new_hyperlink_lines(lines)
}

#[cfg(test)]
/// Render MCP tools grouped by connection using the fully-qualified tool names.
pub(crate) fn new_mcp_tools_output(
    config: &Config,
    tools: HashMap<String, codex_protocol::mcp::Tool>,
    resources: HashMap<String, Vec<Resource>>,
    resource_templates: HashMap<String, Vec<ResourceTemplate>>,
    auth_statuses: &HashMap<String, McpAuthStatus>,
) -> PlainHistoryCell {
    let mut lines: Vec<Line<'static>> = vec![
        "/mcp".magenta().into(),
        "".into(),
        vec!["🔌  ".into(), "MCP Tools".bold()].into(),
        "".into(),
    ];

    if tools.is_empty() {
        lines.push("  • No MCP tools available.".italic().into());
        lines.push("".into());
    }

    let effective_servers = config.mcp_servers.get().clone();
    let mut servers: Vec<_> = effective_servers.iter().collect();
    servers.sort_by_key(|(server, _)| *server);

    for (server, cfg) in servers {
        let prefix = qualified_mcp_tool_name_prefix(server);
        let mut names: Vec<String> = tools
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .map(|k| k[prefix.len()..].to_string())
            .collect();
        names.sort();

        let auth_status = auth_statuses
            .get(server.as_str())
            .copied()
            .unwrap_or(McpAuthStatus::Unsupported);
        let mut header: Vec<Span<'static>> = vec!["  • ".into(), server.clone().into()];
        if !cfg.enabled {
            header.push(" ".into());
            header.push("(disabled)".red());
            lines.push(header.into());
            if let Some(reason) = cfg.disabled_reason.as_ref().map(ToString::to_string) {
                lines.push(vec!["    • Reason: ".into(), reason.dim()].into());
            }
            lines.push(Line::from(""));
            continue;
        }
        lines.push(header.into());
        lines.push(vec!["    • Status: ".into(), "enabled".green()].into());
        lines.push(
            vec![
                "    • Auth: ".into(),
                mcp_auth_status_label(auth_status).into(),
            ]
            .into(),
        );

        match &cfg.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                env,
                env_vars,
                cwd,
            } => {
                let args_suffix = if args.is_empty() {
                    String::new()
                } else {
                    format!(" {}", args.join(" "))
                };
                let cmd_display = format!("{command}{args_suffix}");
                lines.push(vec!["    • Command: ".into(), cmd_display.into()].into());

                if let Some(cwd) = cwd.as_ref() {
                    lines.push(vec!["    • Cwd: ".into(), cwd.to_string().into()].into());
                }

                let env_display = format_env_display(env.as_ref(), env_vars);
                if env_display != "-" {
                    lines.push(vec!["    • Env: ".into(), env_display.into()].into());
                }
            }
            McpServerTransportConfig::StreamableHttp {
                url,
                http_headers,
                env_http_headers,
                ..
            } => {
                lines.push(vec!["    • URL: ".into(), url.clone().into()].into());
                if let Some(headers) = http_headers.as_ref()
                    && !headers.is_empty()
                {
                    let mut pairs: Vec<_> = headers.iter().collect();
                    pairs.sort_by_key(|(name, _)| *name);
                    let display = pairs
                        .into_iter()
                        .map(|(name, _)| format!("{name}=*****"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    lines.push(vec!["    • HTTP headers: ".into(), display.into()].into());
                }
                if let Some(headers) = env_http_headers.as_ref()
                    && !headers.is_empty()
                {
                    let mut pairs: Vec<_> = headers.iter().collect();
                    pairs.sort_by_key(|(name, _)| *name);
                    let display = pairs
                        .into_iter()
                        .map(|(name, var)| format!("{name}={var}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    lines.push(vec!["    • Env HTTP headers: ".into(), display.into()].into());
                }
            }
        }

        if names.is_empty() {
            lines.push("    • Tools: (none)".into());
        } else {
            lines.push(vec!["    • Tools: ".into(), names.join(", ").into()].into());
        }

        let server_resources: Vec<Resource> =
            resources.get(server.as_str()).cloned().unwrap_or_default();
        if server_resources.is_empty() {
            lines.push("    • Resources: (none)".into());
        } else {
            let mut spans: Vec<Span<'static>> = vec!["    • Resources: ".into()];

            for (idx, resource) in server_resources.iter().enumerate() {
                if idx > 0 {
                    spans.push(", ".into());
                }

                let label = resource.title.as_ref().unwrap_or(&resource.name);
                spans.push(label.clone().into());
                spans.push(" ".into());
                spans.push(format!("({})", resource.uri).dim());
            }

            lines.push(spans.into());
        }

        let server_templates: Vec<ResourceTemplate> = resource_templates
            .get(server.as_str())
            .cloned()
            .unwrap_or_default();
        if server_templates.is_empty() {
            lines.push("    • Resource templates: (none)".into());
        } else {
            let mut spans: Vec<Span<'static>> = vec!["    • Resource templates: ".into()];

            for (idx, template) in server_templates.iter().enumerate() {
                if idx > 0 {
                    spans.push(", ".into());
                }

                let label = template.title.as_ref().unwrap_or(&template.name);
                spans.push(label.clone().into());
                spans.push(" ".into());
                spans.push(format!("({})", template.uri_template).dim());
            }

            lines.push(spans.into());
        }

        lines.push(Line::from(""));
    }

    PlainHistoryCell::new(lines)
}

/// Build the `/mcp` history cell from app-server `McpServerStatus` responses.
///
/// The server list comes directly from the app-server status response, sorted
/// alphabetically. The TUI deliberately does not enrich these rows from
/// client-local config because the app-server owns the remote MCP state.
///
/// Normal output is a compact connection summary. Full detail preserves the
/// tool, auth, resource, and resource-template inventory.
pub(crate) fn new_mcp_tools_output_from_statuses(
    statuses: &[McpServerStatus],
    detail: McpServerStatusDetail,
) -> PlainHistoryCell {
    let mut lines: Vec<Line<'static>> = vec![
        "/mcp".magenta().into(),
        "".into(),
        vec!["🔌  ".into(), "MCP Tools".bold()].into(),
        "".into(),
    ];

    let mut statuses = statuses.iter().collect::<Vec<_>>();
    statuses.sort_by(|a, b| (&a.name, &a.environment_id).cmp(&(&b.name, &b.environment_id)));

    let has_any_tools = statuses.iter().any(|status| !status.tools.is_empty());
    if !has_any_tools && matches!(detail, McpServerStatusDetail::Full) {
        lines.push("  • No MCP tools available.".italic().into());
        lines.push("".into());
    }

    for status in statuses {
        let (label, style) = match status.runtime_status {
            Some(McpServerConnectionStatus::Connected) => {
                ("connected", status_style(StatusTone::Success))
            }
            Some(McpServerConnectionStatus::Starting) => ("starting", accent_style()),
            Some(McpServerConnectionStatus::AuthenticationRequired) => (
                "authentication required",
                status_style(StatusTone::Attention),
            ),
            Some(McpServerConnectionStatus::Failed) => {
                ("failed", status_style(StatusTone::Failure))
            }
            Some(McpServerConnectionStatus::NotStarted) => ("not started", Style::default().dim()),
            Some(McpServerConnectionStatus::Disabled) => ("disabled", Style::default().dim()),
            Some(McpServerConnectionStatus::Cancelled) => ("cancelled", Style::default().dim()),
            None if matches!(
                status.auth_status,
                codex_app_server_protocol::McpAuthStatus::NotLoggedIn
            ) =>
            {
                (
                    "authentication required",
                    status_style(StatusTone::Attention),
                )
            }
            None => ("unknown", Style::default().dim()),
        };
        let count = status.tools.len();
        let unit = if count == 1 { "tool" } else { "tools" };
        let name = match &status.environment_id {
            Some(environment_id) => format!("{} (environment {environment_id})", status.name),
            None => status.name.clone(),
        };
        lines.push(
            vec![
                "  • ".set_style(style),
                name.bold(),
                ": ".into(),
                label.set_style(style),
                format!(" ({count} {unit})").dim(),
            ]
            .into(),
        );
        if matches!(detail, McpServerStatusDetail::ToolsAndAuthOnly) {
            continue;
        }
        let auth_status = match status.auth_status {
            codex_app_server_protocol::McpAuthStatus::Unknown => McpAuthStatus::Unknown,
            codex_app_server_protocol::McpAuthStatus::Unsupported => McpAuthStatus::Unsupported,
            codex_app_server_protocol::McpAuthStatus::NotLoggedIn => McpAuthStatus::NotLoggedIn,
            codex_app_server_protocol::McpAuthStatus::BearerToken => McpAuthStatus::BearerToken,
            codex_app_server_protocol::McpAuthStatus::OAuth => McpAuthStatus::OAuth,
        };
        lines.push(
            vec![
                "    • Auth: ".into(),
                mcp_auth_status_label(auth_status).into(),
            ]
            .into(),
        );

        let mut names = status.tools.keys().cloned().collect::<Vec<_>>();
        names.sort();
        if names.is_empty() {
            lines.push("    • Tools: (none)".into());
        } else {
            lines.push(vec!["    • Tools: ".into(), names.join(", ").into()].into());
        }

        if matches!(detail, McpServerStatusDetail::Full) {
            let server_resources = status.resources.clone();
            if server_resources.is_empty() {
                lines.push("    • Resources: (none)".into());
            } else {
                let mut spans: Vec<Span<'static>> = vec!["    • Resources: ".into()];

                for (idx, resource) in server_resources.iter().enumerate() {
                    if idx > 0 {
                        spans.push(", ".into());
                    }

                    let label = resource.title.as_ref().unwrap_or(&resource.name);
                    spans.push(label.clone().into());
                    spans.push(" ".into());
                    spans.push(format!("({})", resource.uri).dim());
                }

                lines.push(spans.into());
            }

            let server_templates = status.resource_templates.clone();
            if server_templates.is_empty() {
                lines.push("    • Resource templates: (none)".into());
            } else {
                let mut spans: Vec<Span<'static>> = vec!["    • Resource templates: ".into()];

                for (idx, template) in server_templates.iter().enumerate() {
                    if idx > 0 {
                        spans.push(", ".into());
                    }

                    let label = template.title.as_ref().unwrap_or(&template.name);
                    spans.push(label.clone().into());
                    spans.push(" ".into());
                    spans.push(format!("({})", template.uri_template).dim());
                }

                lines.push(spans.into());
            }
        }

        lines.push(Line::from(""));
    }

    if matches!(detail, McpServerStatusDetail::ToolsAndAuthOnly) {
        lines.push("".into());
        lines.push("  Use /mcp verbose for tools and resources.".dim().into());
    }

    PlainHistoryCell::new(lines)
}
/// A transient history cell that shows an animated spinner while the MCP
/// inventory RPC is in flight.
///
/// Inserted as the `active_cell` by `ChatWidget::add_mcp_output()` and removed
/// once the fetch completes. The app removes committed copies from transcript
/// history, while `ChatWidget::clear_mcp_inventory_loading()` only clears the
/// in-flight `active_cell`.
#[derive(Debug, Clone)]
pub(crate) struct McpInventoryLoadingCell {
    start_time: Instant,
    animations_enabled: bool,
}

impl McpInventoryLoadingCell {
    pub(crate) fn new(animations_enabled: bool) -> Self {
        Self {
            start_time: Instant::now(),
            animations_enabled,
        }
    }
}

impl HistoryCell for McpInventoryLoadingCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![
            vec![
                activity_indicator(
                    Some(self.start_time),
                    MotionMode::from_animations_enabled(self.animations_enabled),
                    ReducedMotionIndicator::StaticBullet,
                )
                .unwrap_or_else(|| "•".dim()),
                " ".into(),
                "Loading MCP inventory".bold(),
                "…".dim(),
            ]
            .into(),
        ]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![Line::from("Loading MCP inventory...")]
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        if !self.animations_enabled {
            return None;
        }
        Some((self.start_time.elapsed().as_millis() / 50) as u64)
    }
}

/// Convenience constructor for [`McpInventoryLoadingCell`].
pub(crate) fn new_mcp_inventory_loading(animations_enabled: bool) -> McpInventoryLoadingCell {
    McpInventoryLoadingCell::new(animations_enabled)
}
fn format_mcp_invocation(invocation: &McpInvocation) -> Line<'_> {
    let args_str = invocation
        .arguments
        .as_ref()
        .map(|v: &serde_json::Value| {
            // Use compact form to keep things short but readable.
            serde_json::to_string(v).unwrap_or_else(|_| v.to_string())
        })
        .unwrap_or_default();

    let mut invocation_spans = vec![
        invocation.server.as_str().fg(accent_color()),
        ".".into(),
        invocation.tool.as_str().fg(accent_color()),
        "(".into(),
        args_str.dim(),
        ")".into(),
    ];
    if let Some(environment_id) = &invocation.environment_id {
        invocation_spans.push(format!(" (environment {environment_id})").dim());
    }
    invocation_spans.into()
}
