//! Independent disclosures within one tool entry. Targets survive wrapping and completion.

use super::ActivityDisclosure;
use crate::terminal_hyperlinks::HyperlinkLine;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum FoldTarget {
    #[default]
    Activity,
    Part(String),
    OutputMode(super::output_provenance::OutputMode),
}

impl FoldTarget {
    pub(crate) fn ids(&self, activity: &[String]) -> Vec<String> {
        match self {
            Self::Activity | Self::OutputMode(_) => activity.to_vec(),
            Self::Part(id) => vec![id.clone()],
        }
    }
}

pub(crate) struct PartControl {
    pub(crate) id: String,
    pub(crate) line: usize,
    pub(crate) expanded: bool,
}

#[derive(Default)]
pub(crate) struct ActivityParts {
    pub(crate) lines: Vec<HyperlinkLine>,
    pub(crate) header: Option<ActivityHeader>,
    pub(crate) output: Option<ActivityDisclosure>,
    pub(crate) controls: Vec<PartControl>,
}

/// Inline output-control anchor supplied by the header's owner.
#[derive(Clone, Copy)]
pub(crate) struct ActivityHeader {
    /// Byte offset in selectable header source, excluding synthetic marker/timestamp prefixes.
    pub(crate) source_offset: usize,
    /// Clipping an invocation can reveal more arguments; a fixed label cannot.
    pub(crate) expand_on_truncation: bool,
}

impl ActivityHeader {
    pub(crate) fn at_end(line: &ratatui::text::Line<'_>) -> Self {
        Self {
            source_offset: line.spans.iter().map(|span| span.content.len()).sum(),
            expand_on_truncation: true,
        }
    }
}
