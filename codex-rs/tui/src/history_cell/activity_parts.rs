//! Independent disclosures within one tool entry. Targets survive wrapping and completion.

use super::ActivityDisclosure;
use crate::terminal_hyperlinks::HyperlinkLine;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum FoldTarget {
    #[default]
    Activity,
    Part(String),
}

impl FoldTarget {
    pub(crate) fn ids(&self, activity: &[String]) -> Vec<String> {
        match self {
            Self::Activity => activity.to_vec(),
            Self::Part(id) => vec![id.clone()],
        }
    }
}

pub(crate) struct PartControl {
    pub(crate) id: String,
    pub(crate) line: usize,
    pub(crate) expanded: bool,
}

pub(crate) struct ActivityParts {
    pub(crate) lines: Vec<HyperlinkLine>,
    pub(crate) header: Option<ActivityHeader>,
    pub(crate) output: Option<ActivityDisclosure>,
    pub(crate) controls: Vec<PartControl>,
}

/// Token insertion anchor supplied by the header's owner, never inferred from rendered styling.
#[derive(Clone, Copy)]
pub(crate) struct ActivityHeader {
    /// Byte offset in selectable header source, excluding synthetic marker/timestamp prefixes.
    pub(crate) source_offset: usize,
    /// Visible columns before the estimate, excluding the outer disclosure gutter.
    pub(crate) columns: usize,
}

impl ActivityHeader {
    pub(crate) fn at_end(line: &ratatui::text::Line<'_>) -> Self {
        Self {
            source_offset: line.spans.iter().map(|span| span.content.len()).sum(),
            columns: line.width(),
        }
    }
}

pub(crate) fn token_label(tokens: u64) -> String {
    let tokens = codex_protocol::num_format::format_with_separators(
        i64::try_from(tokens).unwrap_or(i64::MAX),
    );
    format!(" (~ {tokens} tokens)")
}
