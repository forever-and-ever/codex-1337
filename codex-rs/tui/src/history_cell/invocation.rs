//! Invocation projections. Visibility belongs to the reader, never to a retained cell.

use super::CodeModeCell;
use super::HistoryCell;
use super::ToolItemCell;
use super::ToolOutputCell;
use super::ToolOutputGroup;
use super::activity_parts::ActivityParts;
use super::output_provenance::ContextAuthor;
use super::output_provenance::OutputMode;
use super::output_provenance::OutputProvenance;
use super::output_provenance::captured_lines;
use super::output_provenance::client_notice;
use super::output_provenance::mark_lines;
use crate::exec_cell::ExecCall;
use crate::exec_cell::ExecCell;
use crate::terminal_hyperlinks::HyperlinkLine;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::tool_input::ToolInput;
use ratatui::style::Stylize;
use ratatui::text::Line;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Port {
    Input,
    Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Toggle(Port, Panel),
    Format(Port, OutputMode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Panel {
    Hidden(OutputMode),
    Visible(OutputMode),
}

impl Panel {
    pub(crate) fn mode(self) -> OutputMode {
        match self {
            Self::Hidden(mode) | Self::Visible(mode) => mode,
        }
    }
    pub(crate) fn visible(self) -> bool {
        matches!(self, Self::Visible(_))
    }
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Hidden(mode) => Self::Visible(mode),
            Self::Visible(mode) => Self::Hidden(mode),
        }
    }
}

pub(crate) enum Source<'a> {
    Code(&'a CodeModeCell),
    Command(&'a ExecCall),
    Cells(Vec<&'a dyn HistoryCell>),
    Output,
}

pub(crate) struct Invocation<'a> {
    pub(crate) id: &'a str,
    pub(crate) source: Source<'a>,
    pub(crate) outputs: Vec<&'a ToolOutputCell>,
}

#[derive(Clone)]
pub(crate) struct ActivityIndicator {
    pub(crate) line: usize,
    pub(crate) start: std::time::Instant,
    pub(crate) mode: crate::motion::MotionMode,
}

#[derive(Clone, Default)]
pub(crate) struct InvocationInput {
    pub(crate) parts: ActivityParts,
    pub(crate) indicators: Vec<ActivityIndicator>,
}

impl From<ActivityParts> for InvocationInput {
    fn from(parts: ActivityParts) -> Self {
        Self {
            parts,
            indicators: Vec::new(),
        }
    }
}

impl InvocationInput {
    #[expect(
        clippy::expect_used,
        reason = "A started activity with the static reduced-motion bullet always has a visible marker."
    )]
    pub(crate) fn paint(&self) -> ActivityParts {
        let mut parts = self.parts.clone();
        for indicator in &self.indicators {
            let Some(line) = parts.lines.get_mut(indicator.line) else {
                continue;
            };
            let Some(span) = line.line.spans.first_mut() else {
                continue;
            };
            let next = crate::motion::activity_indicator(
                Some(indicator.start),
                indicator.mode,
                crate::motion::ReducedMotionIndicator::StaticBullet,
            )
            .expect("activity marker is visible in both motion modes");
            if let Some(source) = &mut line.source {
                source.prefix_bytes =
                    source.prefix_bytes.saturating_sub(span.content.len()) + next.content.len();
            }
            *span = next;
        }
        parts
    }
}

fn combine_inputs(parts: impl IntoIterator<Item = InvocationInput>) -> InvocationInput {
    let mut indicators = Vec::new();
    let mut offset = 0;
    let parts = combine_parts(parts.into_iter().map(|mut input| {
        for indicator in &mut input.indicators {
            indicator.line += offset;
        }
        offset += input.parts.lines.len();
        indicators.extend(input.indicators);
        input.parts
    }));
    InvocationInput { parts, indicators }
}

fn read_input(input: &ToolInput) -> Option<ActivityParts> {
    let read = codex_shell_command::read_intent::ReadIntent::from_tool_input(input)?;
    let mut lines = vec![crate::exec_cell::rows::read_summary(&read.label(&read.path)).into()];
    mark_lines(&mut lines, OutputProvenance::Formatted);
    Some(ActivityParts {
        lines,
        ..Default::default()
    })
}

fn write_input(input: &ToolInput) -> Option<ActivityParts> {
    if !input.name.is_default_namespace() || input.name.name != "fs" {
        return None;
    }
    let arguments: serde_json::Value = serde_json::from_str(input.payload.text()).ok()?;
    if arguments.get("operation")?.as_str()? != "write" {
        return None;
    }
    let path = arguments.get("path")?.as_str()?;
    if path.is_empty() {
        return None;
    }
    Some(ActivityParts {
        lines: captured_lines(
            &format!("Write {path}"),
            ContextAuthor::Environment,
            OutputMode::Fmt,
        ),
        ..Default::default()
    })
}

fn command_input(input: &ToolInput) -> Option<ActivityParts> {
    if !input.name.is_default_namespace() {
        return None;
    }
    let mut lines = match input.name.name.as_str() {
        "bash" => crate::exec_cell::rows::formatted_input(input.payload.text())?,
        "exec_command" => {
            let arguments: serde_json::Value = serde_json::from_str(input.payload.text()).ok()?;
            crate::exec_cell::rows::formatted_input(arguments.get("cmd")?.as_str()?)?
        }
        _ => return None,
    };
    mark_lines(&mut lines, OutputProvenance::Formatted);
    Some(ActivityParts {
        lines,
        ..Default::default()
    })
}

fn argument_input(input: Option<&ToolInput>) -> Option<ActivityParts> {
    let input = input?;
    if let Some(parts) = read_input(input)
        .or_else(|| write_input(input))
        .or_else(|| command_input(input))
    {
        return Some(parts);
    }
    let codex_protocol::tool_input::ToolInputPayload::Json(source) = &input.payload else {
        return None;
    };
    let _ = super::output_provenance::format_json(source).ok()??;
    Some(ActivityParts {
        lines: captured_lines(source, ContextAuthor::Environment, OutputMode::Fmt),
        ..Default::default()
    })
}

fn combine_parts(parts: impl IntoIterator<Item = ActivityParts>) -> ActivityParts {
    let mut combined = ActivityParts::default();
    for mut part in parts {
        for control in &mut part.controls {
            control.line += combined.lines.len();
        }
        combined.controls.extend(part.controls);
        combined.lines.extend(part.lines);
    }
    combined
}

fn edit_projection(
    cell: &dyn HistoryCell,
    width: u16,
    expanded: &std::collections::HashSet<String>,
    full: bool,
) -> ActivityParts {
    let any = cell.as_any();
    if let Some(wrapper) = any.downcast_ref::<ToolItemCell>() {
        return edit_projection(wrapper.inner.as_ref(), width, expanded, full);
    }
    if let Some(mcp) = any.downcast_ref::<super::McpToolCallCell>() {
        return mcp.receipt_parts(width, expanded, full);
    }
    if let Some(patch) = any.downcast_ref::<super::PatchHistoryCell>() {
        let all;
        let expanded = if full {
            all = expanded
                .iter()
                .cloned()
                .chain(patch.activity_ids())
                .collect();
            &all
        } else {
            expanded
        };
        return patch.nested_summary_parts(width, expanded);
    }
    let members: Vec<&dyn HistoryCell> = if let Some(group) = any.downcast_ref::<ToolOutputGroup>()
    {
        group.members.iter().map(AsRef::as_ref).collect()
    } else if let Some(block) = any.downcast_ref::<CodeModeCell>() {
        block.members().iter().map(AsRef::as_ref).collect()
    } else if let Some(computer) = any.downcast_ref::<super::ComputerActivityCell>() {
        computer
            .group
            .calls
            .iter()
            .map(|call| call as &dyn HistoryCell)
            .chain(computer.group.details.members())
            .collect()
    } else if let Some(exec) = any.downcast_ref::<ExecCell>() {
        exec.group.details.members().collect()
    } else {
        Vec::new()
    };
    combine_parts(
        members
            .into_iter()
            .map(|member| edit_projection(member, width, expanded, full)),
    )
}

fn input_projection(cell: &dyn HistoryCell, width: u16) -> Option<InvocationInput> {
    let any = cell.as_any();
    if let Some(input) = any.downcast_ref::<super::ToolInputCell>() {
        if input.0.name.is_default_namespace() && input.0.name.name == "update_work_status" {
            // Activity owns this call's presentation outside the script.
            return Some(InvocationInput::default());
        }
        return read_input(&input.0)
            .or_else(|| write_input(&input.0))
            .or_else(|| command_input(&input.0))
            .map(Into::into);
    }
    if let Some(wrapper) = any.downcast_ref::<ToolItemCell>() {
        return input_projection(wrapper.inner.as_ref(), width);
    }
    if let Some(group) = any.downcast_ref::<ToolOutputGroup>() {
        return Some(combine_inputs(
            group
                .members
                .iter()
                .filter(|cell| !cell.as_any().is::<ToolOutputCell>())
                .map(|cell| input_projection(cell.as_ref(), width))
                .collect::<Option<Vec<_>>>()?,
        ));
    }
    // Edits have their own structural projection, independent of the source ports.
    if any.is::<super::PatchHistoryCell>() {
        return Some(InvocationInput::default());
    }
    let lines = if let Some(exec) = any.downcast_ref::<ExecCell>() {
        exec.iter_calls()
            .flat_map(|call| {
                let source = crate::exec_command::strip_bash_lc_and_escape(&call.command);
                crate::exec_cell::rows::formatted_input(&source).unwrap_or_default()
            })
            .collect()
    } else if let Some(mcp) = any.downcast_ref::<super::McpToolCallCell>() {
        return Some(mcp.invocation_input(width));
    } else if let Some(dynamic) = any.downcast_ref::<super::DynamicToolCallCell>() {
        dynamic.invocation_input(width)
    } else if any.is::<crate::multi_agents::AgentToolHistoryCell>() {
        cell.nested_summary_lines(width)
    } else if any.is::<super::WebSearchCell>() {
        cell.nested_detail_lines(width)
    } else {
        return None;
    };
    Some(
        ActivityParts {
            lines,
            ..Default::default()
        }
        .into(),
    )
}

pub(crate) fn collect(cell: &dyn HistoryCell) -> Vec<Invocation<'_>> {
    fn visit<'a>(
        cell: &'a dyn HistoryCell,
        calls: &mut Vec<Invocation<'a>>,
        outputs: &mut Vec<&'a ToolOutputCell>,
    ) {
        let any = cell.as_any();
        if let Some(block) = any.downcast_ref::<CodeModeCell>() {
            calls.push(Invocation {
                id: block.exec_call_id(),
                source: Source::Code(block),
                outputs: block
                    .members()
                    .iter()
                    .filter_map(|cell| cell.as_any().downcast_ref::<ToolOutputCell>())
                    .collect(),
            });
        } else if let Some(group) = any.downcast_ref::<ToolOutputGroup>() {
            for member in &group.members {
                visit(member.as_ref(), calls, outputs);
            }
        } else if let Some(output) = any.downcast_ref::<ToolOutputCell>() {
            outputs.push(output);
        } else if let Some(exec) = any.downcast_ref::<ExecCell>() {
            calls.extend(exec.iter_calls().map(|call| Invocation {
                id: &call.call_id,
                source: Source::Command(call),
                outputs: Vec::new(),
            }));
            for member in exec.group.details.members() {
                visit(member, calls, outputs);
            }
        } else if let Some(computer) = any.downcast_ref::<super::ComputerActivityCell>() {
            for call in &computer.group.calls {
                visit(call, calls, outputs);
            }
            for member in computer.group.details.members() {
                visit(member, calls, outputs);
            }
        } else if let Some(&id) = cell.tool_call_ids().first() {
            let source = any
                .downcast_ref::<ToolItemCell>()
                .map_or(cell, |wrapper| wrapper.inner.as_ref());
            if let Some(Invocation {
                source: Source::Cells(members),
                ..
            }) = calls.iter_mut().find(|call| call.id == id)
            {
                members.push(source);
            } else {
                calls.push(Invocation {
                    id,
                    source: Source::Cells(vec![source]),
                    outputs: Vec::new(),
                });
            }
        }
    }
    let mut calls = Vec::new();
    let mut outputs = Vec::new();
    visit(cell, &mut calls, &mut outputs);
    for output in outputs {
        let id = output.outer_call_id().unwrap_or_else(|| output.id());
        if let Some(call) = calls.iter_mut().find(|call| call.id == id) {
            if !call.outputs.iter().any(|old| old.id() == output.id()) {
                call.outputs.push(output);
            }
        } else {
            calls.push(Invocation {
                id,
                source: Source::Output,
                outputs: vec![output],
            });
        }
    }
    calls
}

impl Invocation<'_> {
    pub(crate) fn name<'a>(
        &'a self,
        input: Option<&'a ToolInput>,
    ) -> Option<&'a codex_protocol::ToolName> {
        input
            .map(|input| &input.name)
            .or_else(|| self.outputs.first().map(|output| output.name()))
    }

    pub(crate) fn is_active(&self) -> bool {
        match &self.source {
            Source::Code(block) => block.is_active(),
            Source::Command(call) => call.duration.is_none(),
            Source::Cells(cells) => cells
                .iter()
                .any(|cell| super::has_running_tool_calls(*cell)),
            Source::Output => false,
        }
    }

    pub(crate) fn title(&self, input: Option<&ToolInput>) -> Line<'static> {
        let title = if let Source::Code(block) = self.source {
            format!("Code Mode ({})", block.tetrad())
        } else if let Some(language) = self.script_language(input) {
            format!("Script ({language})")
        } else {
            match self
                .name(input)
                .filter(|name| name.is_default_namespace())
                .map(|name| name.name.as_str())
            {
                Some("fence") => "Fence".into(),
                _ => self
                    .name(input)
                    .map_or_else(|| "Command".into(), |name| format!("Command ({name})")),
            }
        };
        let mut title = Line::from(title.bold());
        match &self.source {
            Source::Code(block) => match block.status().map(|(status, _)| status) {
                Some(codex_protocol::code_mode::CodeModeStatus::Failed) => {
                    title.push_span(" · failed".red());
                }
                Some(codex_protocol::code_mode::CodeModeStatus::Interrupted) => {
                    title.push_span(" · interrupted".red());
                }
                None if block.is_active() => title.push_span(" · running".dim()),
                _ => {}
            },
            Source::Command(call) => match &call.output {
                None if call.start_time.is_some() => title.push_span(" · running".dim()),
                Some(output) => match output.outcome {
                    crate::exec_cell::CommandOutcome::Exited(0) => {}
                    crate::exec_cell::CommandOutcome::Exited(code) => {
                        title.push_span(format!(" · failed ({code})").red());
                    }
                    crate::exec_cell::CommandOutcome::Declined => {
                        title.push_span(" · declined".red());
                    }
                },
                _ => {}
            },
            Source::Cells(cells) => {
                let failed = cells.iter().any(|cell| {
                    cell.as_any()
                        .downcast_ref::<super::McpToolCallCell>()
                        .is_some_and(|cell| cell.success() == Some(false))
                        || cell
                            .as_any()
                            .downcast_ref::<super::DynamicToolCallCell>()
                            .is_some_and(super::DynamicToolCallCell::failed)
                });
                let patch = cells.iter().find_map(|cell| {
                    cell.as_any()
                        .downcast_ref::<super::PatchHistoryCell>()
                        .and_then(super::PatchHistoryCell::failure_label)
                });
                if let Some(status) = cells.iter().find_map(|cell| {
                    cell.as_any()
                        .downcast_ref::<crate::multi_agents::AgentToolHistoryCell>()
                        .and_then(crate::multi_agents::AgentToolHistoryCell::invocation_status)
                }) {
                    title.push_span(format!(" · {status}").dim());
                } else if failed {
                    title.push_span(" · failed".red());
                } else if let Some(label) = patch {
                    title.push_span(format!(" · {label}").red());
                } else if cells
                    .iter()
                    .any(|cell| super::has_running_tool_calls(*cell))
                {
                    title.push_span(" · running".dim());
                }
            }
            Source::Output => {}
        }
        if !self.outputs.is_empty()
            && self
                .outputs
                .iter()
                .all(|output| output.is_empty_completed_return())
        {
            title.push_span(" · no output".dim());
        }
        title
    }

    pub(crate) fn script_language(&self, input: Option<&ToolInput>) -> Option<&'static str> {
        if matches!(self.source, Source::Code(_)) {
            return Some("javascript");
        }
        match self
            .name(input)
            .filter(|name| name.is_default_namespace())
            .map(|name| name.name.as_str())
        {
            Some("bash") => Some("bash"),
            Some("python") => Some("python"),
            _ => None,
        }
    }

    pub(crate) fn raw_source<'a>(&'a self, input: Option<&'a ToolInput>) -> Option<&'a str> {
        input
            .map(|input| input.payload.text())
            .or_else(|| match self.source {
                Source::Code(block) => block.script(),
                _ => None,
            })
    }

    pub(crate) fn raw_input(&self, input: Option<&ToolInput>) -> Option<Vec<HyperlinkLine>> {
        let _stage = crate::ui_latency::enter(crate::ui_latency::Stage::InvocationPreparation);
        let source = self.raw_source(input)?;
        let Some(language) = self
            .script_language(input)
            .filter(|_| !source.chars().any(super::output_provenance::unsafe_control))
        else {
            return Some(captured_lines(
                source,
                ContextAuthor::Environment,
                OutputMode::Raw,
            ));
        };
        let highlighted = crate::render::highlight::highlight_code_to_lines(source, language);
        // Highlighting changes paint, never the captured source or its final empty line.
        let mut lines = crate::terminal_hyperlinks::plain_hyperlink_lines(
            source
                .split('\n')
                .enumerate()
                .map(|(index, text)| {
                    highlighted
                        .get(index)
                        .filter(|line| line.to_string() == text)
                        .cloned()
                        .unwrap_or_else(|| Line::from(text.to_owned()))
                })
                .collect(),
        );
        mark_lines(&mut lines, OutputProvenance::Captured);
        Some(lines)
    }

    pub(crate) fn formatted_input(
        &self,
        input: Option<&ToolInput>,
        width: u16,
    ) -> Option<InvocationInput> {
        let _stage = crate::ui_latency::enter(crate::ui_latency::Stage::InvocationPreparation);
        let mut parts = match &self.source {
            Source::Code(block) => {
                let mut projections = block.members().iter().filter_map(|cell| {
                    if let Some(input) = cell.as_any().downcast_ref::<super::ToolInputCell>() {
                        return input_projection(cell.as_ref(), width).or_else(|| {
                            block.members().iter().find_map(|member| {
                                (!member.as_any().is::<super::ToolInputCell>()
                                    && member.tool_call_ids().contains(&input.0.call_id.as_str()))
                                .then(|| input_projection(member.as_ref(), width))
                                .flatten()
                            })
                        });
                    }
                    // Captures supply exact arguments, not a replacement renderer. Render
                    // their native semantic cell at the capture's position, once.
                    if cell.tool_call_ids().iter().any(|id| {
                        block.members().iter().any(|member| {
                            member
                                .as_any()
                                .downcast_ref::<super::ToolInputCell>()
                                .is_some_and(|input| input.0.call_id == *id)
                        })
                    }) {
                        return None;
                    }
                    input_projection(cell.as_ref(), width)
                });
                let first = projections.next()?;
                combine_inputs(std::iter::once(first).chain(projections))
            }
            Source::Command(call) => {
                let script = input
                    .filter(|input| input.name.is_default_namespace() && input.name.name == "bash")
                    .map_or_else(
                        || crate::exec_command::strip_bash_lc_and_escape(&call.command),
                        |input| input.payload.text().to_owned(),
                    );
                let lines = match crate::exec_cell::rows::formatted_input(&script) {
                    Some(lines) => lines,
                    None if input.is_none() => {
                        let mut lines =
                            vec![client_notice("Stored execution · exact input unavailable")];
                        lines.extend(super::output_provenance::client_lines(
                            &script,
                            OutputMode::Fmt,
                        ));
                        return Some(
                            ActivityParts {
                                lines,
                                ..Default::default()
                            }
                            .into(),
                        );
                    }
                    None => return None,
                };
                ActivityParts {
                    lines,
                    ..Default::default()
                }
                .into()
            }
            Source::Cells(cells) => match cells
                .iter()
                .map(|cell| input_projection(*cell, width))
                .collect::<Option<Vec<_>>>()
            {
                Some(parts) => combine_inputs(parts),
                None => return argument_input(input).map(Into::into),
            },
            Source::Output => return argument_input(input).map(Into::into),
        };
        if parts.parts.lines.is_empty() && !matches!(self.source, Source::Code(_)) {
            return None;
        }
        mark_lines(&mut parts.parts.lines, OutputProvenance::Formatted);
        Some(parts)
    }

    pub(crate) fn output_ids(&self) -> Vec<String> {
        if self.outputs.is_empty() {
            vec![format!("call:{}", self.id)]
        } else {
            self.outputs
                .iter()
                .flat_map(|output| output.activity_ids())
                .collect()
        }
    }

    pub(crate) fn edit_parts(
        &self,
        width: u16,
        expanded: &std::collections::HashSet<String>,
        full: bool,
    ) -> ActivityParts {
        let mut parts = match &self.source {
            Source::Code(block) => edit_projection(*block, width, expanded, full),
            Source::Cells(cells) => combine_parts(
                cells
                    .iter()
                    .map(|cell| edit_projection(*cell, width, expanded, full)),
            ),
            Source::Command(_) | Source::Output => ActivityParts::default(),
        };
        mark_lines(&mut parts.lines, OutputProvenance::Client);
        parts
    }

    pub(crate) fn has_formatted_output(&self, width: u16) -> bool {
        if !self.outputs.is_empty() {
            return self.outputs.iter().any(|output| {
                let reformats = |source: &str| {
                    super::output_provenance::format_json(source)
                        .ok()
                        .flatten()
                        .is_some_and(|formatted| formatted != source)
                };
                match output.captured_output() {
                    FunctionCallOutputBody::Text(text) => reformats(text),
                    FunctionCallOutputBody::ContentItems(items) => items.iter().any(|item| {
                        matches!(item, FunctionCallOutputContentItem::InputText { text }
                            if reformats(text))
                    }),
                }
            });
        }
        let raw = self.output(width, OutputMode::Raw);
        let formatted = self.output(width, OutputMode::Fmt);
        !raw.iter()
            .map(|line| line.line.to_string())
            .eq(formatted.iter().map(|line| line.line.to_string()))
    }

    pub(crate) fn output(&self, width: u16, mode: OutputMode) -> Vec<HyperlinkLine> {
        let _stage = crate::ui_latency::enter(crate::ui_latency::Stage::InvocationPreparation);
        if self.outputs.is_empty() {
            let mut lines = vec![client_notice("Exact output is unavailable in this record")];
            if mode == OutputMode::Fmt {
                let mut stored = match &self.source {
                    Source::Command(call) => call.output.as_ref().map_or_else(Vec::new, |output| {
                        super::output_provenance::client_lines(
                            &output.transcript_lines().collect::<Vec<_>>().join("\n"),
                            mode,
                        )
                    }),
                    Source::Cells(cells) => cells
                        .iter()
                        .flat_map(|cell| {
                            cell.as_any()
                                .downcast_ref::<super::McpToolCallCell>()
                                .map_or_else(
                                    || {
                                        if cell.as_any().is::<super::PatchHistoryCell>() {
                                            Vec::new()
                                        } else {
                                            cell.expanded_output_lines(width, mode)
                                        }
                                    },
                                    |mcp| mcp.stored_output_lines(width),
                                )
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                mark_lines(&mut stored, OutputProvenance::Client);
                lines.extend(stored);
            }
            return lines;
        }
        let mut lines = Vec::new();
        for output in &self.outputs {
            match output.captured_output() {
                FunctionCallOutputBody::Text(text) => {
                    lines.extend(captured_lines(text, ContextAuthor::Environment, mode));
                }
                FunctionCallOutputBody::ContentItems(items) => {
                    for item in items {
                        match item {
                            FunctionCallOutputContentItem::InputText { text } => {
                                lines.extend(captured_lines(
                                    text,
                                    ContextAuthor::Environment,
                                    mode,
                                ));
                            }
                            FunctionCallOutputContentItem::InputImage { .. } => {
                                lines.push(client_notice("Image attachment"));
                            }
                            FunctionCallOutputContentItem::InputAudio { .. } => {
                                lines.push(client_notice("Audio attachment"));
                            }
                            FunctionCallOutputContentItem::EncryptedContent { .. } => {
                                lines.push(client_notice("Encrypted content"));
                            }
                        }
                    }
                }
            }
        }
        if lines.is_empty() {
            lines.push(client_notice("No output"));
        }
        lines
    }
}
