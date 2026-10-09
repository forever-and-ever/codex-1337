//! Shell execution has one row pipeline. Named primitives are optional labels on source spans.

use super::model::ExecCall;
use super::model::ExecCell;
use super::segments::Connector;
use super::segments::ShellSegment;
use super::segments::shell_segments;
use super::segments::summarize_shell_segment;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell::ActivityDisclosure;
use crate::history_cell::HistoryCell;
use crate::history_cell::ToolOutputCell;
use crate::history_cell::activity_parts::ActivityHeader;
use crate::history_cell::activity_parts::ActivityParts;
use crate::history_cell::activity_parts::PartControl;
use crate::history_cell::activity_preview::compact_header;
use crate::history_cell::output_provenance::OutputMode;
use crate::history_cell::output_provenance::OutputProvenance;
use crate::history_cell::output_provenance::client_notice;
use crate::history_cell::output_provenance::mark_lines;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::render::highlight::highlight_bash_to_lines;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;
use codex_protocol::parse_command::ParsedCommand;
use codex_shell_command::read_intent::ReadIntent;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct CommandRows {
    script: String,
    rows: Vec<CommandRow>,
}

#[derive(Debug)]
struct CommandRow {
    segment: ShellSegment,
    summary: Option<Line<'static>>,
}

impl ExecCall {
    fn command_rows(&self) -> &CommandRows {
        self.rows.get_or_init(|| {
            let script = strip_bash_lc_and_escape(&self.command);
            let rows = shell_segments(&script)
                .into_iter()
                .map(|segment| {
                    let source = script[segment.source.clone()].trim();
                    let summary = match summarize_shell_segment(source) {
                        ParsedCommand::Read { name, .. } => Some(
                            vec![
                                "Read ".cyan(),
                                ReadIntent::parse(source)
                                    .map_or(name.clone(), |read| read.label(&name))
                                    .into(),
                            ]
                            .into(),
                        ),
                        ParsedCommand::ListFiles { path, .. } => Some(
                            vec!["List ".cyan(), path.unwrap_or_else(|| ".".into()).into()].into(),
                        ),
                        ParsedCommand::Search { query, path, .. } => Some(
                            vec![
                                "Search ".cyan(),
                                query.unwrap_or_default().into(),
                                path.map_or_else(String::new, |path| format!(" in {path}"))
                                    .into(),
                            ]
                            .into(),
                        ),
                        ParsedCommand::Unknown { .. } => None,
                    };
                    CommandRow { segment, summary }
                })
                .collect();
            Arc::new(CommandRows { script, rows })
        })
    }
}

impl ExecCell {
    /// Final presentation label only; grouping and lifecycle never depend on recognition.
    pub(crate) fn has_only_read_actions(&self) -> bool {
        self.group.calls.iter().all(|call| {
            call.command_rows()
                .rows
                .iter()
                .all(|row| row.summary.is_some())
        })
    }
}

pub(crate) fn command_header(cells: &[&ExecCell]) -> (Line<'static>, ActivityHeader) {
    let active = cells.iter().any(|cell| cell.is_active());
    let failed = cells.iter().flat_map(|cell| cell.iter_calls()).any(|call| {
        call.output
            .as_ref()
            .is_some_and(|output| output.exit_code != 0)
    });
    let read_only = cells.iter().all(|cell| cell.has_only_read_actions());
    let marker = if active {
        activity_indicator(
            cells.iter().find_map(|cell| cell.active_start_time()),
            MotionMode::from_animations_enabled(cells.iter().any(|cell| cell.animations_enabled())),
            ReducedMotionIndicator::StaticBullet,
        )
        .unwrap_or_else(|| "•".dim())
    } else if failed {
        "•".red().bold()
    } else {
        "•".green().bold()
    };
    let verb = match (active, read_only) {
        (true, true) => "Exploring",
        (true, false) => "Running",
        (false, true) => "Explored",
        (false, false) => "Ran",
    };
    let mut heading = Line::from(vec![marker, " ".into()]);
    heading.push_span(verb.bold());
    let header = ActivityHeader::at_end(&heading);
    if failed {
        heading.push_span(" · failed".red());
    }
    (heading, header)
}

pub(crate) fn command_parts(
    cells: &[&ExecCell],
    batches: &[&ToolOutputCell],
    width: u16,
    header_reserve: usize,
    expanded: &HashSet<String>,
    activity_ids: &[String],
    mode: OutputMode,
) -> ActivityParts {
    let calls = cells
        .iter()
        .flat_map(|cell| cell.iter_calls())
        .collect::<Vec<_>>();
    let (heading, header) = command_header(cells);
    let has_batches = !batches.is_empty()
        || cells
            .iter()
            .any(|cell| cell.group.details.returned_output_has_details().is_some());
    let has_output = if has_batches {
        batches
            .iter()
            .any(|batch| batch.activity_disclosure(width).is_some())
            || cells
                .iter()
                .any(|cell| cell.group.details.returned_output_has_details() == Some(true))
    } else {
        calls.iter().any(|call| {
            call.output
                .as_ref()
                .is_some_and(|output| output.lines().any(|line| !line.trim().is_empty()))
        })
    };
    let mut lines = vec![HyperlinkLine::from(heading.clone())];
    let mut controls = Vec::new();
    let row_id = |call: &ExecCall, row: &CommandRow| {
        format!(
            "command:{}:{}-{}",
            call.call_id, row.segment.source.start, row.segment.source.end
        )
    };
    let inline = calls.len() == 1
        && calls[0]
            .command_rows()
            .rows
            .iter()
            .all(|row| row.summary.is_none() && !expanded.contains(&row_id(calls[0], row)))
        && !calls[0].command_rows().script.contains('\n')
        && heading.width()
            + 1
            + Line::from(calls[0].command_rows().script.as_str()).width()
            + header_reserve
            <= usize::from(width);
    if inline {
        lines[0].line.push_span(" ");
        lines[0].line.extend(
            highlight_bash_to_lines(&calls[0].command_rows().script)
                .into_iter()
                .flat_map(|line| line.spans),
        );
    } else {
        for call in &calls {
            let rows = call.command_rows();
            for row in &rows.rows {
                let source = rows.script[row.segment.source.clone()].trim();
                if source.is_empty() {
                    continue;
                }
                let id = row_id(call, row);
                let open = row.summary.is_none() && expanded.contains(&id);
                let connector = row.segment.connector.map_or("", Connector::suffix);
                let available = usize::from(width).saturating_sub(
                    usize::from(crate::transcript_geometry::CHILD_INDENT) + connector.len(),
                );
                let mut source_lines = row
                    .summary
                    .clone()
                    .map_or_else(|| highlight_bash_to_lines(source), |summary| vec![summary]);
                let preview = source_lines.first().cloned().unwrap_or_default();
                let clipped = preview.width() > available || source_lines.len() > 1;
                if row.summary.is_none() && (open || clipped) {
                    controls.push(PartControl {
                        id,
                        line: lines.len(),
                        expanded: open,
                    });
                }
                if open {
                    if let Some(last) = source_lines.last_mut() {
                        last.push_span(connector.dim());
                    }
                    let mut detail = plain_hyperlink_lines(source_lines);
                    mark_lines(&mut detail, OutputProvenance::Client);
                    lines.extend(prefix_hyperlink_lines(
                        detail,
                        crate::transcript_geometry::CHILD_PREFIX.into(),
                        crate::transcript_geometry::CHILD_PREFIX.into(),
                    ));
                } else {
                    let mut preview = compact_header(preview, available);
                    preview.push_span(connector.dim());
                    lines.extend(prefix_hyperlink_lines(
                        vec![preview.into()],
                        crate::transcript_geometry::CHILD_PREFIX.into(),
                        crate::transcript_geometry::CHILD_PREFIX.into(),
                    ));
                }
            }
        }
    }
    if has_output && activity_ids.iter().any(|id| expanded.contains(id)) {
        lines.push("".into());
        if has_batches {
            for cell in cells {
                lines.extend(
                    cell.group
                        .details
                        .output_lines(width, mode)
                        .unwrap_or_default(),
                );
            }
            for batch in batches {
                lines.extend(batch.output_lines(width, mode));
            }
        } else {
            lines.push(client_notice("  Stored result · exact capture unavailable"));
            for call in calls {
                if let Some(output) = &call.output {
                    let mut detail = plain_hyperlink_lines(
                        output
                            .transcript_lines()
                            .map(|line| codex_ansi_escape::ansi_escape_line(line.as_ref()))
                            .collect(),
                    );
                    mark_lines(&mut detail, OutputProvenance::Client);
                    lines.extend(prefix_hyperlink_lines(
                        detail,
                        crate::transcript_geometry::CHILD_PREFIX.into(),
                        crate::transcript_geometry::CHILD_PREFIX.into(),
                    ));
                }
            }
        }
    }
    ActivityParts {
        lines,
        header: Some(header),
        output: has_output.then_some(ActivityDisclosure::Generic),
        controls,
    }
}
