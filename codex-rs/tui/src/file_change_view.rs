//! Per-file presentation shared by native changes and producer-reported MCP changes.

use std::path::PathBuf;

use ratatui::prelude::*;
use ratatui::style::Stylize;

use crate::diff_model::FileChange;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::terminal_hyperlinks::HyperlinkLine;

#[derive(Clone, Debug)]
pub(crate) struct FileChangeView<'a> {
    pub(crate) label: String,
    /// Used only to choose syntax highlighting, never to read or open a file.
    pub(crate) syntax_path: PathBuf,
    pub(crate) change: Option<&'a FileChange>,
    pub(crate) qualifier: Option<&'static str>,
}

impl FileChangeView<'_> {
    pub(crate) fn summary(&self, width: usize) -> HyperlinkLine {
        let mut suffix = Vec::new();
        if let Some(change) = &self.change {
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
        let label: String = self.label.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let mut line = truncate_line_with_ellipsis_if_overflow(
            Line::from(label),
            width.saturating_sub(suffix.width()),
        );
        line.extend(suffix.spans);
        truncate_line_with_ellipsis_if_overflow(line, width).into()
    }

    pub(crate) fn body(&self, width: usize) -> Vec<HyperlinkLine> {
        self.change.as_ref().map_or_else(Vec::new, |change| {
            crate::diff_render::file_change_lines(change, &self.syntax_path, width)
        })
    }
}
