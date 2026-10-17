//! Display projections never replace captured tool text.

use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::style::Style;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputMode {
    #[default]
    Raw,
    Fmt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputProvenance {
    #[default]
    Unspecified,
    Captured,
    Formatted,
    Client,
    /// Human-only diff paint is semantic, not a captured-content background.
    ClientDiff,
    /// Client serialization of model-bound definitions, never the provider's hidden prompt.
    Definitions(OutputMode),
}

/// Authorship is independent of inclusion and capture representation.
#[derive(Clone, Copy)]
pub(crate) enum ContextAuthor {
    User,
    Assistant,
    Subagent,
    Environment,
}

impl ContextAuthor {
    pub(crate) fn style(self) -> Style {
        match self {
            Self::User => crate::style::history_prompt_style(),
            Self::Assistant => crate::style::history_response_style(),
            Self::Subagent => crate::style::subagent_message_style(),
            Self::Environment => crate::style::context_message_style(),
        }
    }
}

pub(crate) fn captured_lines(
    text: &str,
    author: ContextAuthor,
    mode: OutputMode,
) -> Vec<HyperlinkLine> {
    let formatted = (mode == OutputMode::Fmt)
        .then(|| format_json(text).ok().flatten())
        .flatten();
    let shown = formatted.as_deref().unwrap_or(text);
    let escaped = shown.chars().any(unsafe_control).then(|| {
        shown.chars().fold(String::new(), |mut output, ch| {
            if unsafe_control(ch) {
                output.extend(ch.escape_default());
            } else {
                output.push(ch);
            }
            output
        })
    });
    let shown = escaped.as_deref().unwrap_or(shown);
    let mut lines = crate::terminal_hyperlinks::plain_hyperlink_lines(
        shown
            .split('\n')
            .map(|line| ratatui::text::Line::from(line.to_owned()))
            .collect(),
    );
    let provenance = if mode == OutputMode::Fmt || escaped.is_some() {
        OutputProvenance::Formatted
    } else {
        OutputProvenance::Captured
    };
    mark_lines(&mut lines, provenance);
    if provenance == OutputProvenance::Captured {
        for line in &mut lines {
            let style = author.style();
            line.line.style = line.line.style.patch(style);
            if let Some(source) = &mut line.source {
                source.line_style = source.line_style.patch(style);
            }
        }
    }
    if escaped.is_some() {
        lines.push(client_notice("Control characters escaped for display"));
    }
    lines
}

pub(crate) fn captured_blocks(
    blocks: &[String],
    author: ContextAuthor,
    mode: OutputMode,
) -> Vec<HyperlinkLine> {
    let mut lines = Vec::new();
    for (index, text) in blocks.iter().enumerate() {
        if blocks.len() > 1 {
            lines.push(client_notice(&format!("Text block {}", index + 1)));
        }
        lines.extend(captured_lines(text, author, mode));
    }
    lines
}

/// Legacy mixed captures have unknown inclusion boundaries; do not guess model authorship.
pub(crate) fn client_lines(text: &str, mode: OutputMode) -> Vec<HyperlinkLine> {
    let mut lines = captured_lines(text, ContextAuthor::Environment, mode);
    for line in &mut lines {
        line.line.style = Style::default();
        if let Some(source) = &mut line.source {
            source.line_style = Style::default();
        }
    }
    mark_lines(&mut lines, OutputProvenance::Client);
    if mode == OutputMode::Fmt {
        for line in &mut lines {
            line.line.style = line
                .line
                .style
                .patch(crate::style::formatted_output_style());
            if let Some(source) = &mut line.source {
                source.line_style = source
                    .line_style
                    .patch(crate::style::formatted_output_style());
            }
        }
    }
    lines
}

pub(crate) fn context_sources(
    sources: &[codex_protocol::ContextSource],
    mode: OutputMode,
) -> Vec<HyperlinkLine> {
    use codex_protocol::ContextSource;
    let mut lines = Vec::new();
    for source in sources {
        let (heading, text) = match source {
            ContextSource::Prompt { label, text } => (label.as_str(), text.as_str()),
            ContextSource::ToolDeclarations { text } => (
                "Tool Declarations — submitted definitions (client JSON); provider rendering unavailable",
                text.as_str(),
            ),
        };
        if !lines.is_empty() {
            lines.push(client_notice(""));
        }
        let heading = super::sanitize_user_text(heading.into()).replace(['\n', '\t'], " ");
        lines.push(client_notice(&heading));
        let mut body = captured_lines(text, ContextAuthor::Environment, mode);
        if matches!(source, ContextSource::ToolDeclarations { .. }) {
            for line in &mut body {
                let provenance = match line.source.as_ref().map(|source| source.output_provenance) {
                    Some(OutputProvenance::Captured) => {
                        OutputProvenance::Definitions(OutputMode::Raw)
                    }
                    Some(OutputProvenance::Formatted) => {
                        OutputProvenance::Definitions(OutputMode::Fmt)
                    }
                    _ => continue,
                };
                mark_lines(std::slice::from_mut(line), provenance);
            }
        }
        lines.extend(body);
    }
    lines
}

pub(crate) fn raw_cell_lines(cell: &dyn super::HistoryCell, width: u16) -> Vec<HyperlinkLine> {
    let any = cell.as_any();
    let members = any
        .downcast_ref::<super::CodeModeCell>()
        .map(|group| group.members())
        .or_else(|| {
            any.downcast_ref::<super::ToolOutputGroup>()
                .map(|group| group.members.as_slice())
        });
    if let Some(members) = members {
        return members
            .iter()
            .flat_map(|member| raw_cell_lines(member.as_ref(), width))
            .collect();
    }
    if let Some(preamble) = any.downcast_ref::<super::PreambleCell>() {
        return preamble.source_lines(OutputMode::Raw);
    }
    if let Some(arrival) = any.downcast_ref::<super::ArrivalCell>() {
        return arrival.context_lines(width, OutputMode::Raw);
    }
    if let Some(output) = any.downcast_ref::<super::ToolOutputCell>() {
        return output.output_lines(width, OutputMode::Raw);
    }
    let author = if any.is::<super::UserHistoryCell>() {
        Some(ContextAuthor::User)
    } else if any.is::<super::AgentMessageCell>()
        || any.is::<super::AgentMarkdownCell>()
        || any.is::<super::StreamingAgentTailCell>()
        || any.is::<super::ReasoningSummaryCell>()
        || any.is::<super::ProposedPlanCell>()
        || any.is::<super::ProposedPlanStreamCell>()
        || any.is::<super::StreamingPlanTailCell>()
    {
        Some(ContextAuthor::Assistant)
    } else {
        // Thought's raw projection is only a generated heading, not its reasoning body.
        None
    };
    let mut lines = crate::terminal_hyperlinks::plain_hyperlink_lines(cell.live_raw_lines());
    if let Some(author) = author {
        for line in &mut lines {
            line.line.style = line.line.style.patch(author.style());
            if let Some(source) = &mut line.source {
                source.line_style = source.line_style.patch(author.style());
            }
        }
    } else {
        mark_lines(&mut lines, OutputProvenance::Client);
    }
    lines
}

impl OutputProvenance {
    fn style(self) -> Style {
        match self {
            Self::Unspecified | Self::ClientDiff => Style::default(),
            Self::Captured | Self::Definitions(OutputMode::Raw) => {
                crate::style::context_message_style()
            }
            Self::Formatted | Self::Definitions(OutputMode::Fmt) => Self::Client
                .style()
                .patch(crate::style::formatted_output_style()),
            Self::Client => Style::default()
                .bg(ratatui::style::Color::Reset)
                .remove_modifier(ratatui::style::Modifier::REVERSED),
        }
    }
}

pub(crate) fn client_notice(text: &str) -> HyperlinkLine {
    let indent = text.len() - text.trim_start_matches(' ').len();
    let mut line = HyperlinkLine::from(ratatui::text::Line::from(text.to_owned()));
    let mut source = crate::terminal_hyperlinks::LogicalLineSource::new(text[indent..].to_owned());
    source.prefix_bytes = indent;
    source.continuation_indent = text[..indent].to_owned().into();
    line.source = Some(source);
    mark_lines(std::slice::from_mut(&mut line), OutputProvenance::Client);
    line
}

pub(crate) fn mark_lines(lines: &mut [HyperlinkLine], provenance: OutputProvenance) {
    for line in lines {
        // A container cannot repaint the diff's additions, deletions, or reflow source.
        if line
            .source
            .as_ref()
            .is_some_and(|source| source.output_provenance == OutputProvenance::ClientDiff)
        {
            continue;
        }
        let style = provenance.style();
        line.line.style = line.line.style.patch(style);
        let source = line.source.get_or_insert_with(|| {
            crate::terminal_hyperlinks::LogicalLineSource::from_line(&line.line)
        });
        let clear_client_background = source.output_provenance == OutputProvenance::Client
            && provenance != OutputProvenance::Client;
        source.output_provenance = provenance;
        source.line_style = source.line_style.patch(style);
        if style.bg == Some(ratatui::style::Color::Reset) {
            // Reset every paint layer, without erasing semantic foreground colors.
            let style = OutputProvenance::Client.style();
            for span in &mut line.line.spans {
                span.style = span.style.patch(style);
            }
            for (_, span_style) in std::sync::Arc::make_mut(&mut source.styles) {
                *span_style = span_style.patch(style);
            }
            source.span_style = source.span_style.patch(style);
        } else if clear_client_background {
            // The new line provenance owns its fill; an earlier client projection's
            // explicit Reset must not override it in the source spans.
            for span in &mut line.line.spans {
                span.style.bg = None;
            }
            for (_, span_style) in std::sync::Arc::make_mut(&mut source.styles) {
                span_style.bg = None;
            }
            source.span_style.bg = None;
        }
    }
}

pub(super) const INPUT_LIMIT: usize = 256 * 1024;
pub(super) const OUTPUT_LIMIT: usize = 1024 * 1024;
pub(super) struct ProjectionLimit;

pub(super) fn unsafe_control(ch: char) -> bool {
    ch.is_control() && !matches!(ch, '\n' | '\t')
}

/// Validate without a DOM, then edit only framing whitespace and value whitespace escapes.
/// A limit failure rejects the entire capture projection, not a truncated or partial result.
#[expect(
    clippy::expect_used,
    reason = "Validated JSON and the byte loop prove a character exists at each visited string position"
)]
pub(super) fn format_json(source: &str) -> Result<Option<String>, ProjectionLimit> {
    const DEPTH_LIMIT: usize = 128;
    if source.len() > INPUT_LIMIT {
        return Err(ProjectionLimit);
    }
    // Mandatory visible escaping must not be hidden by optional JSON whitespace formatting.
    if source.chars().any(unsafe_control) {
        return Ok(None);
    }
    let source = source.trim_matches([' ', '\n', '\r', '\t']);
    if !matches!(source.as_bytes().first(), Some(b'{' | b'[')) {
        return Ok(None);
    }
    // Validation visits no DOM: duplicate keys and numeric lexemes remain in source.
    if serde_json::from_str::<serde::de::IgnoredAny>(source).is_err() {
        return Ok(None);
    }
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut depth = 0usize;
    let mut index = 0;
    let mut in_string = false;
    let mut string_value = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if byte == b'\\' {
                let escape = bytes[index + 1];
                let decoded = match (string_value, escape) {
                    (true, b'n' | b'f') => Some(('\n', 2)),
                    (true, b'r') => Some((
                        '\n',
                        if source[index..].starts_with(r"\r\n") {
                            4
                        } else {
                            2
                        },
                    )),
                    (true, b't') => Some(('\t', 2)),
                    (true, b'u') => u32::from_str_radix(&source[index + 2..index + 6], 16)
                        .ok()
                        .and_then(char::from_u32)
                        .filter(|ch| ch.is_whitespace())
                        .map(|ch| {
                            (
                                if (ch.is_control() && ch != '\t')
                                    || matches!(ch, '\u{2028}' | '\u{2029}')
                                {
                                    '\n'
                                } else {
                                    ch
                                },
                                6,
                            )
                        }),
                    _ => None,
                };
                if let Some((text, length)) = decoded {
                    output.push(text);
                    index += length;
                } else {
                    let length = if escape == b'u' { 6 } else { 2 };
                    output.push_str(&source[index..index + length]);
                    index += length;
                }
            } else {
                let ch = source[index..]
                    .chars()
                    .next()
                    .expect("validated string has a character");
                output.push(ch);
                index += ch.len_utf8();
                in_string = byte != b'"';
            }
        } else {
            match byte {
                b'"' => {
                    output.push('"');
                    in_string = true;
                    // A key follows an object opener/comma; a value follows ':' or '['.
                    // Scan its closing quote, skipping complete escapes, to distinguish keys.
                    let mut end = index + 1;
                    while bytes[end] != b'"' {
                        end += if bytes[end] == b'\\' { 2 } else { 1 };
                    }
                    string_value =
                        bytes[end + 1..].iter().find(|b| !b.is_ascii_whitespace()) != Some(&b':');
                }
                b'{' | b'[' => {
                    depth += 1;
                    if depth > DEPTH_LIMIT {
                        return Err(ProjectionLimit);
                    }
                    output.push(char::from(byte));
                    if !matches!(
                        bytes[index + 1..].iter().find(|b| !b.is_ascii_whitespace()),
                        Some(b'}' | b']')
                    ) {
                        output.push('\n');
                        output.extend(std::iter::repeat_n(' ', depth * 2));
                    }
                }
                b'}' | b']' => {
                    depth -= 1;
                    if !matches!(
                        bytes[..index]
                            .iter()
                            .rev()
                            .find(|b| !b.is_ascii_whitespace()),
                        Some(b'{' | b'[')
                    ) {
                        output.push('\n');
                        output.extend(std::iter::repeat_n(' ', depth * 2));
                    }
                    output.push(char::from(byte));
                }
                b',' => {
                    output.push_str(",\n");
                    output.extend(std::iter::repeat_n(' ', depth * 2));
                }
                b':' => output.push_str(": "),
                b' ' | b'\n' | b'\r' | b'\t' => {}
                _ => output.push(char::from(byte)),
            }
            index += 1;
        }
        if output.len() > OUTPUT_LIMIT {
            return Err(ProjectionLimit);
        }
    }
    Ok((output != source).then_some(output))
}
