//! Shared bounds for owned-transcript action previews; expansion uses retained source content.

use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::text::Line;
use serde::Serialize;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use std::io::Write;

pub(crate) const DETAIL_PREVIEW_LINES: usize = 3;

/// Details an activity can reveal without materializing its expanded transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivityDisclosure {
    Generic,
    /// Place the control before this logical source line, outside copied text.
    Inline {
        source_line: usize,
    },
}

/// Clip a preview row without teaching selection to copy text that is currently hidden.
pub(crate) fn clipped_line(line: Line<'static>, width: u16) -> HyperlinkLine {
    truncate_line_with_ellipsis_if_overflow(line, usize::from(width)).into()
}

pub(crate) fn compact_header(line: Line<'static>, width: usize) -> Line<'static> {
    if line.width() <= width {
        return line;
    }
    let mut line = crate::line_truncation::truncate_line_to_width(line, width.saturating_sub(3));
    line.push_span(".".repeat(width.min(3)));
    line
}

pub(crate) fn single_line(text: &str) -> String {
    let text = &text[..text.floor_char_boundary(16 * 1024)];
    super::sanitize_user_text(text.into())
        .chars()
        .take(16 * 1024)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

/// Streaming serialization bounds work and allocation independently of retained arguments.
pub(crate) fn compact_arguments(value: &serde_json::Value) -> String {
    struct Preview<'a>(&'a serde_json::Value, usize);
    impl Serialize for Preview<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            match self.0 {
                _ if self.1 == 32 => serializer.serialize_str("..."),
                serde_json::Value::String(text) if text.starts_with("data:") => {
                    serializer.serialize_str("<media>")
                }
                serde_json::Value::String(text) if text.len() > 16 * 1024 => {
                    let mut prefix = text[..text.floor_char_boundary(16 * 1024)].to_owned();
                    prefix.push_str("...");
                    serializer.serialize_str(&prefix)
                }
                serde_json::Value::Array(items) => {
                    let mut seq = serializer.serialize_seq(Some(items.len()))?;
                    for item in items {
                        seq.serialize_element(&Preview(item, self.1 + 1))?;
                    }
                    seq.end()
                }
                serde_json::Value::Object(fields) => {
                    let mut map = serializer.serialize_map(Some(fields.len()))?;
                    let media = fields
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|kind| {
                            matches!(kind, "image" | "input_image" | "audio" | "input_audio")
                        });
                    for (key, value) in fields {
                        let key = &key[..key.floor_char_boundary(16 * 1024)];
                        if media && key == "data" {
                            map.serialize_entry(key, "<media>")?;
                        } else {
                            map.serialize_entry(key, &Preview(value, self.1 + 1))?;
                        }
                    }
                    map.end()
                }
                value => value.serialize(serializer),
            }
        }
    }
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let available = (16 * 1024usize).saturating_sub(self.0.len());
            let written = bytes.len().min(available);
            self.0.extend_from_slice(&bytes[..written]);
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    let clipped = serde_json::to_writer(&mut output, &Preview(value, 0)).is_err();
    let mut preview = single_line(&String::from_utf8_lossy(&output.0));
    if clipped {
        preview.push_str("...");
    }
    preview
}
