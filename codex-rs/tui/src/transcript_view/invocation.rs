//! One pair of ports per invocation. Structural feed folds do not own port visibility.

use super::TextLayout;
use super::TranscriptView;
use super::text::HeaderControl;
use super::text::HeaderRows;
use crate::history_cell::HistoryCell;
use crate::history_cell::activity_parts::FoldTarget;
use crate::history_cell::invocation::Action;
use crate::history_cell::invocation::Invocation;
use crate::history_cell::invocation::Panel;
use crate::history_cell::invocation::Port;
use crate::history_cell::invocation::collect;
use crate::history_cell::output_provenance::OutputMode;
use crate::history_cell::output_provenance::client_notice;
use crate::terminal_hyperlinks::HyperlinkLine;
use codex_protocol::ToolName;
use codex_protocol::tool_input::ToolInput;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::collections::HashMap;

pub(super) const CONTROL_COLUMNS: usize = "[in [raw] [fmt]] [out [raw] [fmt]]".len();

fn input_open_by_default(name: Option<&ToolName>) -> bool {
    // Tool-specific presentation policy; explicit port choices always win.
    const HIDDEN_INPUT_TOOLS: &[&str] = &[
        "write_stdin",
        "bash",
        "python",
        "request_user_input",
        "request_user_input_async",
    ];
    !name.is_some_and(|name| {
        name.is_default_namespace() && HIDDEN_INPUT_TOOLS.contains(&name.name.as_str())
    })
}

#[derive(Default)]
struct Choices {
    input: Option<Panel>,
    output: Option<Panel>,
}

#[derive(Clone, Copy)]
pub(super) struct Ports {
    input: Panel,
    output: Panel,
}

impl Ports {
    fn panel(self, port: Port) -> Panel {
        match port {
            Port::Input => self.input,
            Port::Output => self.output,
        }
    }
}

#[derive(Default)]
pub(super) struct Invocations {
    inputs: HashMap<String, ToolInput>,
    choices: HashMap<String, Choices>,
}

impl Invocations {
    pub(super) fn input(&self, id: &str) -> Option<&ToolInput> {
        self.inputs.get(id)
    }

    fn ports(
        &self,
        call: &Invocation<'_>,
        open: bool,
        formatted: bool,
        formatted_output: bool,
        full: bool,
    ) -> Ports {
        if full {
            return Ports {
                input: Panel::Visible(
                    if formatted && call.raw_source(self.input(call.id)).is_none() {
                        OutputMode::Fmt
                    } else {
                        OutputMode::Raw
                    },
                ),
                output: Panel::Visible(if call.outputs.is_empty() {
                    OutputMode::Fmt
                } else {
                    OutputMode::Raw
                }),
            };
        }
        let choices = self.choices.get(call.id);
        let mode = if formatted {
            OutputMode::Fmt
        } else {
            OutputMode::Raw
        };
        // A live/page-prefix Code Mode source is not its settled semantic projection.
        // Keep the default body unpainted until arguments or the terminal fact arrive.
        let preferred = choices.and_then(|choice| choice.input).unwrap_or(
            if open
                && input_open_by_default(call.name(self.input(call.id)))
                && (formatted
                    || (!matches!(call.source, crate::history_cell::invocation::Source::Code(block)
                            if block.status().is_none())
                        && call.raw_source(self.input(call.id)).is_some()))
            {
                Panel::Visible(mode)
            } else {
                Panel::Hidden(mode)
            },
        );
        let available = |panel, formatted| match (formatted, panel) {
            (false, Panel::Visible(_)) => Panel::Visible(OutputMode::Raw),
            (false, Panel::Hidden(_)) => Panel::Hidden(OutputMode::Raw),
            (_, panel) => panel,
        };
        Ports {
            input: available(preferred, formatted),
            output: available(
                choices
                    .and_then(|choice| choice.output)
                    .unwrap_or(Panel::Hidden(OutputMode::Fmt)),
                formatted_output,
            ),
        }
    }

    pub(super) fn apply(&mut self, id: &str, action: Action) {
        // A control carries its next complete panel value, so live-to-retained regrouping
        // cannot change the meaning of a click already hit-tested against the old layout.
        let (port, panel) = match action {
            Action::Toggle(port, next) => (port, next),
            Action::Format(port, mode) => (port, Panel::Visible(mode)),
        };
        let ports = self.choices.entry(id.to_owned()).or_default();
        match port {
            Port::Input => ports.input = Some(panel),
            Port::Output => ports.output = Some(panel),
        }
    }

    pub(super) fn header(
        &self,
        call: &Invocation<'_>,
        open: bool,
        width: u16,
        tokens: Option<u64>,
        full: bool,
    ) -> HeaderRows {
        let bill = tokens.map(|tokens| (tokens, super::text::token_label(tokens)));
        let bill_width = bill.as_ref().map_or(0, |(_, label)| label.len() as u16 + 2);
        let width = width.saturating_sub(bill_width).max(1);
        let formatted = call.formatted_input(self.input(call.id), width).is_some();
        let formatted_output = call.has_formatted_output(width);
        let ports = self.ports(call, open, formatted, formatted_output, full);
        let mut title = crate::render::header::paint_work(call.title(self.input(call.id)));
        let mut controls = Vec::new();
        let mut strip = Line::default();
        for (port, label, available) in [
            (Port::Input, "in", formatted),
            (Port::Output, "out", formatted_output),
        ] {
            if !strip.spans.is_empty() {
                strip.push_span(" ");
            }
            let panel = ports.panel(port);
            let start = strip.width() as u16;
            strip.push_span("[");
            strip.push_span(if panel.visible() {
                Span::styled(label, crate::render::disclosure::tab_style())
            } else {
                label.dim()
            });
            controls.push(HeaderControl {
                row: 0,
                columns: start..start + label.len() as u16 + 2,
                target: FoldTarget::Invocation {
                    call_id: call.id.to_owned(),
                    action: Action::Toggle(port, panel.toggled()),
                },
            });
            if panel.visible() {
                for (mode, label) in [(OutputMode::Raw, "raw"), (OutputMode::Fmt, "fmt")] {
                    strip.push_span(" ");
                    let start = strip.width() as u16;
                    let enabled = mode == OutputMode::Raw || available;
                    strip.push_span(if !enabled {
                        Span::raw(" ".repeat(label.len() + 2))
                    } else if panel.mode() == mode {
                        Span::styled(format!("[{label}]"), crate::render::disclosure::tab_style())
                    } else {
                        format!("[{label}]").dim()
                    });
                    if enabled {
                        controls.push(HeaderControl {
                            row: 0,
                            columns: start..strip.width() as u16,
                            target: FoldTarget::Invocation {
                                call_id: call.id.to_owned(),
                                action: Action::Format(port, mode),
                            },
                        });
                    }
                }
                strip.push_span("]");
            } else {
                strip.push_span("]            ");
            }
        }
        let reserve = strip.width() as u16;
        let mut lines = Vec::new();
        let offset = if width > reserve + title.width() as u16 {
            title = crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
                title,
                usize::from(width - reserve - 1),
            );
            let offset = width - reserve;
            title.push_span(" ".repeat(usize::from(offset) - title.width()));
            title.extend(strip.spans);
            lines.push(title);
            offset
        } else {
            lines.push(
                crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
                    title,
                    usize::from(width),
                ),
            );
            if reserve <= width {
                lines.push(strip);
                for control in &mut controls {
                    control.row = 1;
                }
            } else {
                // Each port remains a fixed-width unit; split between ports, never through
                // their hit targets. Very narrow terminals clip unavailable columns.
                let split = 16;
                lines.push(super::text::logical::slice_line(&strip, 0..split));
                lines.push(super::text::logical::slice_line(
                    &strip,
                    split + 1..usize::MAX,
                ));
                for control in &mut controls {
                    if control.columns.start > split as u16 {
                        control.row = 2;
                        control.columns.start -= split as u16 + 1;
                        control.columns.end -= split as u16 + 1;
                    } else {
                        control.row = 1;
                    }
                }
            }
            0
        };
        for control in &mut controls {
            control.columns.start += offset;
            control.columns.end += offset;
        }
        if let Some((tokens, label)) = bill
            && let Some(line) = lines.first_mut()
        {
            line.push_span(" ".repeat(usize::from(width).saturating_sub(line.width()) + 2));
            line.push_span(super::text::token_figure(label, tokens, 0));
        }
        HeaderRows {
            lines,
            fold: None,
            controls,
        }
    }

    pub(super) fn body(
        &self,
        call: &Invocation<'_>,
        open: bool,
        width: u16,
        full: bool,
        expanded: &std::collections::HashSet<String>,
    ) -> crate::history_cell::activity_parts::ActivityParts {
        use crate::history_cell::activity_parts::ActivityParts;
        let input = self.input(call.id);
        let formatted = call.formatted_input(input, width);
        let ports = self.ports(
            call,
            open,
            formatted.is_some(),
            call.has_formatted_output(width),
            full,
        );
        let mut parts = ActivityParts::default();
        if ports.input.visible() {
            parts = if ports.input.mode() == OutputMode::Fmt {
                formatted
            } else {
                call.raw_input(input).map(|lines| ActivityParts {
                    lines,
                    ..Default::default()
                })
            }
            .unwrap_or_else(|| ActivityParts {
                lines: vec![client_notice("Exact input is unavailable in this record")],
                ..Default::default()
            });
        }
        if ports.output.visible() {
            if !parts.lines.is_empty() {
                parts.lines.push(client_notice(""));
            }
            parts.lines.extend(call.output(width, ports.output.mode()));
        }
        let mut edits = call.edit_parts(width, expanded, full);
        if !edits.lines.is_empty() {
            for control in &mut edits.controls {
                control.line += parts.lines.len();
            }
            parts.controls.extend(edits.controls);
            parts.lines.extend(edits.lines);
        }
        parts
    }

    pub(super) fn layout(
        &self,
        cell: &dyn HistoryCell,
        width: u16,
        open: bool,
        placement: super::feed::Content,
        full: bool,
        tokens: &super::tool_output_tokens::ToolOutputTokens,
        expanded: &std::collections::HashSet<String>,
        auxiliary: Vec<HyperlinkLine>,
    ) -> Option<TextLayout> {
        let calls = collect(cell);
        if calls.is_empty() {
            return None;
        }
        let mut lines = Vec::new();
        let mut controls = Vec::new();
        let mut parts = Vec::new();
        let indent = crate::transcript_geometry::TITLE_COLUMN
            + if placement == super::feed::Content::Own {
                crate::transcript_geometry::ENTRY_INDENT
            } else {
                0
            };
        let body_width = width.saturating_sub(indent).max(1);
        for (index, call) in calls.iter().enumerate() {
            if placement != super::feed::Content::Beheaded || index != 0 {
                let header = self.header(
                    call,
                    open && calls.len() == 1,
                    body_width,
                    tokens.for_ids(&call.output_ids()),
                    full,
                );
                let offset = lines.len();
                for mut control in header.controls {
                    control.row += offset;
                    control.columns.start += indent;
                    control.columns.end += indent;
                    controls.push(control);
                }
                lines.extend(header.lines.into_iter().map(|line| {
                    let mut notice = client_notice(&line.to_string());
                    notice.line = line;
                    notice
                }));
            }
            let body_indent = crate::transcript_geometry::ENTRY_INDENT;
            let mut body = self.body(
                call,
                open && calls.len() == 1,
                body_width.saturating_sub(body_indent).max(1),
                full,
                expanded,
            );
            body.lines = crate::transcript_geometry::gutter_lines(body.lines, body_indent);
            for control in &mut body.controls {
                control.line += lines.len();
            }
            parts.extend(body.controls);
            lines.extend(body.lines);
        }
        let clock = (placement == super::feed::Content::Loose)
            .then(|| cell.transcript_timestamp())
            .flatten();
        let mut lines = crate::transcript_geometry::gutter_lines(
            crate::transcript_geometry::clock_lines(lines, clock),
            indent,
        );
        if !auxiliary.is_empty() {
            lines.push(client_notice(""));
            lines.extend(crate::transcript_geometry::rich_lines(auxiliary));
        }
        let mut layout = TextLayout::new(lines, width);
        for control in controls {
            layout = layout.with_invocation_control(control);
        }
        for part in parts {
            layout = layout.with_part_control(part);
        }
        Some(layout)
    }
}

impl TranscriptView {
    pub(crate) fn sync_live_invocation_tail(
        &mut self,
        width: u16,
        key: Option<crate::chatwidget::ActiveCellTranscriptKey>,
        cell: &dyn HistoryCell,
        auxiliary: Vec<HyperlinkLine>,
    ) -> Option<bool> {
        if self.mode != crate::history_cell::HistoryRenderMode::Rich || self.detailed {
            return None;
        }
        if collect(cell).is_empty() {
            return None;
        }
        Some(self.sync_live_layout(width, key, |width, view| {
            view.invocations
                .layout(
                    cell,
                    width,
                    true,
                    super::feed::Content::Loose,
                    false,
                    &view.tool_output_tokens,
                    &view.disclosure.expanded,
                    auxiliary,
                )
                .map(|layout| (layout, true))
        }))
    }

    pub(crate) fn sync_search_invocation_tail(
        &mut self,
        width: u16,
        key: Option<crate::chatwidget::ActiveCellTranscriptKey>,
        cell: &dyn HistoryCell,
        auxiliary: Vec<HyperlinkLine>,
    ) -> Option<bool> {
        if self.mode != crate::history_cell::HistoryRenderMode::Rich || self.detailed {
            return None;
        }
        if !self.search.is_active() {
            return Some(false);
        }
        if collect(cell).is_empty() {
            return None;
        }
        Some(self.sync_search_live_layout(width, key, |width, view| {
            view.invocations.layout(
                cell,
                width,
                true,
                super::feed::Content::Loose,
                true,
                &view.tool_output_tokens,
                &view.disclosure.expanded,
                auxiliary,
            )
        }))
    }

    pub(crate) fn record_tool_input(&mut self, input: ToolInput) {
        if self.invocations.inputs.get(&input.call_id) != Some(&input) {
            self.invocations.inputs.insert(input.call_id.clone(), input);
            self.cache.clear();
            self.live_key = None;
            self.invalidate_live_search();
        }
    }
}
