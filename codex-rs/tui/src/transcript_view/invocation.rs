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

use crate::history_cell::activity_parts::ActivityParts;
use crate::history_cell::activity_parts::PartControl;
use crate::history_cell::invocation::ActivityIndicator;
use crate::history_cell::invocation::InvocationInput;
use crate::history_cell::invocation::Source;
use crate::terminal_hyperlinks::TerminalHyperlink;
use std::cell::Cell;
use std::cell::RefCell;
use std::mem::size_of;
use std::mem::size_of_val;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Weak;

#[path = "invocation_prepare.rs"]
mod preparation;

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

#[derive(Clone, Copy, Default, PartialEq, Eq)]
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

#[derive(Clone, Copy)]
pub(super) enum PreparationSource<'a> {
    Retained(&'a Arc<dyn HistoryCell>),
    Live(u64),
    Uncached,
}

enum SourceRevision {
    Retained(Weak<dyn HistoryCell>),
    Live(u64),
}

impl SourceRevision {
    fn matches(&self, source: PreparationSource<'_>) -> bool {
        match (self, source) {
            (Self::Retained(old), PreparationSource::Retained(source)) => {
                old.ptr_eq(&Arc::downgrade(source))
            }
            (Self::Live(old), PreparationSource::Live(revision)) => *old == revision,
            _ => false,
        }
    }
}

enum PreparedInput {
    Unavailable,
    Cached {
        parts: Rc<InvocationInput>,
        bytes: usize,
    },
    Oversized,
}

fn line_bytes(line: &Line<'_>) -> usize {
    line.spans.capacity() * size_of::<Span<'_>>()
        + line
            .spans
            .iter()
            .map(|span| match &span.content {
                std::borrow::Cow::Owned(text) => text.capacity(),
                std::borrow::Cow::Borrowed(_) => 0,
            })
            .sum::<usize>()
}

fn retained_lines_bytes(lines: &Vec<HyperlinkLine>) -> usize {
    // Copy metadata owns arbitrarily nested Markdown; do not retain it in this cache.
    if lines.iter().any(|line| {
        line.source
            .as_ref()
            .is_some_and(|source| source.copy.is_some())
    }) {
        return usize::MAX;
    }
    lines.capacity() * size_of::<HyperlinkLine>()
        + lines
            .iter()
            .map(|line| {
                line_bytes(&line.line)
                    + line.hyperlinks.capacity() * size_of::<TerminalHyperlink>()
                    + line
                        .hyperlinks
                        .iter()
                        .map(|link| link.destination.capacity())
                        .sum::<usize>()
                    + line.source.as_ref().map_or(0, |source| {
                        source.text.len()
                            + size_of_val(source.styles.as_ref())
                            + line_bytes(&source.continuation_indent)
                    })
            })
            .sum::<usize>()
}

type CachedOutput = (u16, OutputMode, Vec<HyperlinkLine>, usize);

pub(super) struct PreparedInvocation {
    source: Option<SourceRevision>,
    id: String,
    inputs: RefCell<Vec<(u16, PreparedInput)>>,
    retained_bytes: Cell<usize>,
    #[cfg(test)]
    work: Cell<[usize; 4]>,
    raw: RefCell<Option<Vec<HyperlinkLine>>>,
    formatted_output: RefCell<Vec<(u16, bool)>>,
    outputs: RefCell<Vec<CachedOutput>>,
}

impl PreparedInvocation {
    #[cfg(test)]
    fn count_work(&self, index: usize) {
        let mut work = self.work.get();
        work[index] += 1;
        self.work.set(work);
    }

    #[cfg(test)]
    pub(super) fn work(&self) -> [usize; 4] {
        self.work.get()
    }

    fn has_formatted_output(&self, call: &Invocation<'_>, width: u16) -> bool {
        // Captured JSON capability never uses width; stored native output does.
        let key = if call.outputs.is_empty() { width } else { 0 };
        let mut capabilities = self.formatted_output.borrow_mut();
        if let Some((_, available)) = capabilities.iter().find(|(width, _)| *width == key) {
            return *available;
        }
        #[cfg(test)]
        self.count_work(1);
        let available = call.has_formatted_output(width);
        if capabilities.len() == 3 {
            capabilities.remove(0);
        }
        capabilities.push((key, available));
        available
    }

    fn admit(&self, bytes: usize) -> bool {
        // 32 sources × 512 KiB; oversized pane projections are rendered, never retained.
        let total = self.retained_bytes.get().saturating_add(bytes);
        if total > 512 * 1024 {
            return false;
        }
        self.retained_bytes.set(total);
        true
    }

    fn raw_input(
        &self,
        call: &Invocation<'_>,
        input: Option<&ToolInput>,
    ) -> Option<Vec<HyperlinkLine>> {
        if let Some(lines) = self.raw.borrow().as_ref() {
            return Some(lines.clone());
        }
        #[cfg(test)]
        self.count_work(2);
        let lines = call.raw_input(input)?;
        if self.admit(retained_lines_bytes(&lines)) {
            *self.raw.borrow_mut() = Some(lines.clone());
        }
        Some(lines)
    }

    fn input(
        &self,
        call: &Invocation<'_>,
        input: Option<&ToolInput>,
        width: u16,
        materialize: bool,
    ) -> (bool, Option<Rc<InvocationInput>>) {
        let mut inputs = self.inputs.borrow_mut();
        if let Some((_, prepared)) = inputs.iter().find(|(cached, _)| *cached == width) {
            return match prepared {
                PreparedInput::Unavailable => (false, None),
                PreparedInput::Cached { parts, .. } => (true, Some(Rc::clone(parts))),
                PreparedInput::Oversized if !materialize => (true, None),
                PreparedInput::Oversized => {
                    #[cfg(test)]
                    self.count_work(0);
                    (true, call.formatted_input(input, width).map(Rc::new))
                }
            };
        }
        #[cfg(test)]
        self.count_work(0);
        if inputs.len() == 3
            && let (_, PreparedInput::Cached { bytes, .. }) = inputs.remove(0)
        {
            self.retained_bytes.set(self.retained_bytes.get() - bytes);
        }
        // Native renderers own capability detection. Keep their settled result when it fits.
        let parts = call.formatted_input(input, width).map(Rc::new);
        let bytes = parts.as_ref().map_or(0, |parts| {
            retained_lines_bytes(&parts.parts.lines)
                .saturating_add(size_of::<InvocationInput>() + 2 * size_of::<usize>())
                .saturating_add(parts.parts.controls.capacity() * size_of::<PartControl>())
                .saturating_add(
                    parts
                        .parts
                        .controls
                        .iter()
                        .map(|control| control.id.capacity())
                        .sum::<usize>(),
                )
                .saturating_add(parts.indicators.capacity() * size_of::<ActivityIndicator>())
        });
        let prepared = match &parts {
            None => PreparedInput::Unavailable,
            Some(parts) if self.admit(bytes) => PreparedInput::Cached {
                parts: Rc::clone(parts),
                bytes,
            },
            Some(_) => PreparedInput::Oversized,
        };
        inputs.push((width, prepared));
        (parts.is_some(), parts)
    }

    fn output(&self, call: &Invocation<'_>, width: u16, mode: OutputMode) -> Vec<HyperlinkLine> {
        let mut outputs = self.outputs.borrow_mut();
        if let Some((_, _, lines, _)) = outputs
            .iter()
            .find(|(cached, paint, _, _)| *cached == width && *paint == mode)
        {
            return lines.clone();
        }
        #[cfg(test)]
        self.count_work(3);
        let lines = call.output(width, mode);
        if outputs.len() == 6 {
            let (_, _, _, bytes) = outputs.remove(0);
            self.retained_bytes.set(self.retained_bytes.get() - bytes);
        }
        let bytes = retained_lines_bytes(&lines);
        if self.admit(bytes) {
            outputs.push((width, mode, lines.clone(), bytes));
        }
        lines
    }
}

#[derive(Default)]
struct PreparationCache {
    render: Option<super::layout::RenderState>,
    entries: Vec<Rc<PreparedInvocation>>,
}

pub(super) struct InvocationSnapshot {
    inputs: HashMap<String, Arc<ToolInput>>,
    choices: HashMap<String, Choices>,
}

impl InvocationSnapshot {
    pub(super) fn restore(self) -> Invocations {
        Invocations {
            inputs: self.inputs,
            choices: self.choices,
            ..Invocations::default()
        }
    }
}

#[derive(Default)]
pub(super) struct Invocations {
    inputs: HashMap<String, Arc<ToolInput>>,
    revision: u64,
    choices: HashMap<String, Choices>,
    prepared: RefCell<PreparationCache>,
}

impl Invocations {
    pub(super) fn snapshot<'a>(
        &self,
        cells: impl IntoIterator<Item = &'a dyn HistoryCell>,
    ) -> InvocationSnapshot {
        let ids: std::collections::HashSet<_> = cells
            .into_iter()
            .flat_map(collect)
            .map(|call| call.id)
            .collect();
        InvocationSnapshot {
            inputs: ids
                .iter()
                .filter_map(|id| {
                    self.inputs
                        .get(*id)
                        .map(|input| ((*id).to_owned(), Arc::clone(input)))
                })
                .collect(),
            choices: ids
                .iter()
                .filter_map(|id| {
                    self.choices
                        .get(*id)
                        .map(|choice| ((*id).to_owned(), *choice))
                })
                .collect(),
        }
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn prepared(
        &self,
        call: &Invocation<'_>,
        source: PreparationSource<'_>,
    ) -> Rc<PreparedInvocation> {
        // TELOS(fast-ui-thread): source preparation outlives cosmetic animation layouts.
        // Weak ownership prevents address reuse without retaining discarded transcript cells.
        let mut cache = self.prepared.borrow_mut();
        let render = super::layout::RenderState::current();
        if cache.render.as_ref() != Some(&render) {
            cache.entries.clear();
            cache.render = Some(render);
        }
        if let Some(index) = cache.entries.iter().position(|entry| {
            entry.id == call.id && entry.source.as_ref().is_some_and(|old| old.matches(source))
        }) {
            let entry = cache.entries.remove(index);
            cache.entries.push(Rc::clone(&entry));
            return entry;
        }
        let stable = match &call.source {
            Source::Code(block) => block
                .members()
                .iter()
                .all(|cell| cell.has_stable_transcript_height()),
            Source::Cells(cells) => cells.iter().all(|cell| cell.has_stable_transcript_height()),
            _ => true,
        };
        let source = if stable
            && call.id.len() <= 1024
            && !matches!(source, PreparationSource::Retained(owner) if !owner.has_stable_transcript_height())
        {
            source
        } else {
            PreparationSource::Uncached
        };
        let entry = Rc::new(PreparedInvocation {
            source: match source {
                PreparationSource::Retained(source) => {
                    Some(SourceRevision::Retained(Arc::downgrade(source)))
                }
                PreparationSource::Live(revision) => Some(SourceRevision::Live(revision)),
                PreparationSource::Uncached => None,
            },
            id: call.id.to_owned(),
            inputs: RefCell::default(),
            retained_bytes: Cell::default(),
            #[cfg(test)]
            work: Cell::default(),
            raw: RefCell::default(),
            formatted_output: RefCell::default(),
            outputs: RefCell::default(),
        });
        if entry.source.is_some() {
            cache.entries.retain(|entry| !matches!(&entry.source, Some(SourceRevision::Retained(source)) if source.strong_count() == 0));
            if cache.entries.len() == 32 {
                cache.entries.remove(0);
            }
            cache.entries.push(Rc::clone(&entry));
        }
        entry
    }

    pub(super) fn input(&self, id: &str) -> Option<&ToolInput> {
        self.inputs.get(id).map(AsRef::as_ref)
    }

    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "Open/full are independent disclosure policies; formatted input/output independently describe each port capability."
    )]
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
                    || (!matches!(call.source, Source::Code(block)
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
        self.revision = self.revision.wrapping_add(1);
        let ports = self.choices.entry(id.to_owned()).or_default();
        match port {
            Port::Input => ports.input = Some(panel),
            Port::Output => ports.output = Some(panel),
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "The two port strips contain only fixed ASCII labels and delimiters; the complete strip is under 40 columns."
    )]
    pub(super) fn header(
        &self,
        call: &Invocation<'_>,
        prepared: &PreparedInvocation,
        input_width: u16,
        open: bool,
        width: u16,
        tokens: Option<u64>,
        full: bool,
    ) -> HeaderRows {
        let bill = tokens.map(|tokens| (tokens, super::text::token_label(tokens)));
        // The caller supplies the shared title lane; figures occupy the lane after it.
        let width = width.max(1);
        let formatted = prepared
            .input(call, self.input(call.id), input_width, false)
            .0;
        let formatted_output = prepared.has_formatted_output(call, input_width);
        let ports = self.ports(call, open, formatted, formatted_output, full);
        let mut title =
            crate::render::header::paint_work(call.title(self.input(call.id)), call.is_active());
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
        let label_end;
        let offset = if usize::from(width) > usize::from(reserve) + title.width() {
            title = crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
                title,
                usize::from(width - reserve - 1),
            );
            label_end = title.width();
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
            label_end = lines[0].width();
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
            line.push_span(super::text::token_figure(&label, tokens, 0));
        }
        HeaderRows {
            lines: lines
                .into_iter()
                .enumerate()
                .map(|(index, line)| {
                    HeaderRows::label(line, 0..if index == 0 { label_end } else { 0 })
                })
                .collect(),
            fold: None,
            controls,
        }
    }

    pub(super) fn body(
        &self,
        call: &Invocation<'_>,
        prepared: &PreparedInvocation,
        open: bool,
        width: u16,
        full: bool,
        expanded: &std::collections::HashSet<String>,
    ) -> ActivityParts {
        let input = self.input(call.id);
        let formatted = prepared.input(call, input, width, false).0;
        let ports = self.ports(
            call,
            open,
            formatted,
            prepared.has_formatted_output(call, width),
            full,
        );
        let mut parts = ActivityParts::default();
        if ports.input.visible() {
            parts = if ports.input.mode() == OutputMode::Fmt {
                prepared
                    .input(call, input, width, true)
                    .1
                    .map(|input| input.paint())
            } else {
                prepared.raw_input(call, input).map(|lines| ActivityParts {
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
            parts
                .lines
                .extend(prepared.output(call, width, ports.output.mode()));
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
        source: PreparationSource<'_>,
        width: u16,
        open: bool,
        placement: super::feed::Content,
        full: bool,
        tokens: &super::tool_output_tokens::ToolOutputTokens,
        expanded: &std::collections::HashSet<String>,
        auxiliary: Vec<HyperlinkLine>,
    ) -> Option<TextLayout> {
        self.layout_inner(
            cell, source, width, open, placement, full, tokens, expanded, auxiliary,
        )
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "The header lane is explicitly capped by body_width, a u16, immediately before conversion."
    )]
    fn layout_inner(
        &self,
        cell: &dyn HistoryCell,
        source: PreparationSource<'_>,
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
            let prepared = self.prepared(call, source);
            let input_width = body_width
                .saturating_sub(crate::transcript_geometry::ENTRY_INDENT)
                .max(1);
            if placement != super::feed::Content::Beheaded || index != 0 {
                let header = self.header(
                    call,
                    &prepared,
                    input_width,
                    open && calls.len() == 1,
                    super::feed::header_columns(width)
                        .0
                        .saturating_sub(usize::from(
                            indent - crate::transcript_geometry::TITLE_COLUMN,
                        ))
                        .min(usize::from(body_width)) as u16,
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
                lines.extend(header.lines);
            }
            let body_indent = crate::transcript_geometry::ENTRY_INDENT;
            let mut body = self.body(
                call,
                &prepared,
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
        let mut layout = TextLayout::with_invocation_rows(lines, width, controls);
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
        if self.mode != crate::history_cell::HistoryRenderMode::Rich
            || self.detailed
            || collect(cell).is_empty()
        {
            return None;
        }
        let changed = self.sync_live_layout(width, key, |width, view| {
            let source = key.map_or(PreparationSource::Uncached, |key| {
                PreparationSource::Live(key.revision)
            });
            view.invocations
                .layout(
                    cell,
                    source,
                    width,
                    true,
                    super::feed::Content::Loose,
                    false,
                    &view.tool_output_tokens,
                    &view.disclosure.expanded,
                    auxiliary,
                )
                .map(|layout| (Arc::new(layout), true))
        });
        Some(changed)
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
                key.filter(|key| key.cacheable)
                    .map_or(PreparationSource::Uncached, |key| {
                        PreparationSource::Live(key.revision)
                    }),
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
        if self.invocations.input(&input.call_id) != Some(&input) {
            self.invocations.revision = self.invocations.revision.wrapping_add(1);
            let call_id = input.call_id.clone();
            self.invocations
                .inputs
                .insert(call_id.clone(), Arc::new(input));
            self.invocations
                .prepared
                .borrow_mut()
                .entries
                .retain(|entry| entry.id != call_id);
            self.cache.invalidate_call(&call_id);
            self.live_key = None;
            self.invalidate_live_search();
        }
    }
}
