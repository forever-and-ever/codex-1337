//! Display projections never replace captured tool text.

use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::style::Style;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Raw,
    #[default]
    Fmt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputProvenance {
    #[default]
    Unspecified,
    Captured,
    Formatted,
    Client,
}

impl OutputProvenance {
    fn style(self) -> Style {
        match self {
            Self::Unspecified | Self::Captured => Style::default(),
            Self::Formatted => crate::style::formatted_output_style(),
            Self::Client => {
                if crate::terminal_palette::default_bg().is_some() {
                    crate::style::history_prompt_style()
                } else {
                    Style::default().reversed()
                }
            }
        }
    }
}

pub(crate) fn client_notice(text: &str) -> HyperlinkLine {
    let mut line = HyperlinkLine::from(ratatui::text::Line::from(text.to_owned()));
    let mut source = crate::terminal_hyperlinks::LogicalLineSource::new(String::new());
    source.prefix_bytes = text.len();
    line.source = Some(source);
    mark_lines(std::slice::from_mut(&mut line), OutputProvenance::Client);
    line
}

pub(crate) fn mark_lines(lines: &mut [HyperlinkLine], provenance: OutputProvenance) {
    for line in lines {
        let style = provenance.style();
        line.line.style = line.line.style.patch(style);
        let source = line.source.get_or_insert_with(|| {
            crate::terminal_hyperlinks::LogicalLineSource::from_line(&line.line)
        });
        source.output_provenance = provenance;
        source.line_style = source.line_style.patch(style);
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
