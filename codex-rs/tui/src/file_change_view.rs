//! Per-file presentation shared by native changes and producer-reported MCP changes.

use std::borrow::Cow;
use std::fmt::Write;
use std::path::Path;

use ratatui::prelude::*;
use ratatui::style::Stylize;

use crate::diff_model::FileChange;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::terminal_hyperlinks::HyperlinkLine;

#[derive(Clone, Debug)]
pub(crate) struct FileChangeView<'a> {
    pub(crate) label: Cow<'a, str>,
    /// Used only to choose syntax highlighting, never to read or open a file.
    pub(crate) syntax_path: &'a Path,
    pub(crate) change: Option<&'a FileChange>,
    pub(crate) qualifier: Option<&'static str>,
}

impl FileChangeView<'_> {
    pub(crate) fn summary(&self, width: usize) -> HyperlinkLine {
        let mut suffix = Vec::new();
        if let Some(change) = self.change {
            let (added, removed) = crate::diff_render::line_counts(change);
            suffix.extend([
                " (".into(),
                format!("+{added}").green(),
                ", ".into(),
                format!("-{removed}").red(),
                ")".into(),
            ]);
        }
        if let Some(qualifier) = self.qualifier {
            suffix.push(format!(" — {qualifier}").dim());
        }
        let suffix = Line::from(suffix);
        let mut label = String::new();
        for character in self.label.chars() {
            if character.is_control() {
                label.push(' ');
            } else if matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                // Escape text-reordering controls without changing ordinary RTL filenames.
                let _ = write!(label, "\\u{{{:04x}}}", u32::from(character));
            } else {
                label.push(character);
            }
        }
        let mut line = truncate_line_with_ellipsis_if_overflow(
            Line::from(label),
            width.saturating_sub(suffix.width()),
        );
        line.extend(suffix.spans);
        truncate_line_with_ellipsis_if_overflow(line, width).into()
    }

    pub(crate) fn body(&self, width: usize) -> Vec<HyperlinkLine> {
        self.change.map_or_else(Vec::new, |change| {
            crate::diff_render::file_change_lines(change, self.syntax_path, width)
        })
    }
}
