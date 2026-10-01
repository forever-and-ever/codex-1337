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
    pub(crate) output: Option<ActivityDisclosure>,
    pub(crate) controls: Vec<PartControl>,
}

pub(crate) fn token_label(tokens: u64) -> String {
    let tokens = codex_protocol::num_format::format_with_separators(
        i64::try_from(tokens).unwrap_or(i64::MAX),
    );
    format!(" (~ {tokens} tokens)")
}
