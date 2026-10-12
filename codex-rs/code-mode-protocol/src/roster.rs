//! The roster names the nested tools whose declarations `exec`'s description withholds.

use crate::ToolDefinition;
use crate::discovery::compact_tool_summary;
use crate::normalize_code_mode_identifier;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

const HEADER: &str = "These nested tools are not declared here; read a tool's contract with `describe_tool` before its first use.";
const FACTORING_LEGEND: &str = "`a_{b,c}` stands for `a_b` and `a_c`.";

/// A nested tool that `exec`'s description does not declare.
#[derive(Clone, Debug, PartialEq)]
pub enum UndeclaredTool {
    /// Named in the roster beside its purpose summary.
    Glossed(ToolDefinition),
    /// Named in the roster; names sharing a namespace and leading words are factored.
    Named(ToolDefinition),
    /// Neither declared nor named; search finds it.
    Deferred(ToolDefinition),
}

impl UndeclaredTool {
    pub(crate) fn definition(&self) -> &ToolDefinition {
        match self {
            Self::Glossed(definition) | Self::Named(definition) | Self::Deferred(definition) => {
                definition
            }
        }
    }
}

/// Lists each rostered tool's callable name once: glossed tools beside the purpose summary
/// `ALL_TOOLS` reports, then named tools, whose names sharing a namespace and leading words are
/// factored into one line, so a large MCP server costs a few lines instead of its schemas.
pub(crate) fn render_roster(tools: &[UndeclaredTool]) -> Option<String> {
    let mut glossed = BTreeMap::new();
    let mut named = BTreeSet::new();
    for tool in tools {
        match tool {
            UndeclaredTool::Glossed(definition) => {
                glossed.insert(
                    normalize_code_mode_identifier(&definition.name),
                    compact_tool_summary(&definition.description),
                );
            }
            UndeclaredTool::Named(definition) => {
                named.insert(normalize_code_mode_identifier(&definition.name));
            }
            UndeclaredTool::Deferred(_) => {}
        }
    }
    if glossed.is_empty() && named.is_empty() {
        return None;
    }
    let (names, factored) = factor(&named);
    let header = if factored {
        format!("{HEADER} {FACTORING_LEGEND}")
    } else {
        HEADER.to_string()
    };
    let glosses = glossed.into_iter().map(|(name, summary)| {
        if summary.is_empty() {
            format!("- {name}")
        } else {
            format!("- {name}: {summary}")
        }
    });
    Some(
        std::iter::once(header)
            .chain(glosses)
            .chain(names)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Renders one line per name or factored family, reporting whether any family was factored.
fn factor(names: &BTreeSet<String>) -> (Vec<String>, bool) {
    let mut families = BTreeMap::<(&str, &str), Vec<Vec<&str>>>::new();
    for name in names {
        let (namespace, rest) = name
            .rfind("__")
            .map_or(("", name.as_str()), |at| name.split_at(at + 2));
        let words = rest.split('_').collect::<Vec<_>>();
        families
            .entry((namespace, words[0]))
            .or_default()
            .push(words);
    }
    let mut lines = Vec::new();
    let mut factored = false;
    for ((namespace, _), members) in families {
        // Every member keeps at least one word after the factored prefix.
        let shared = members
            .iter()
            .map(|words| {
                words
                    .iter()
                    .zip(&members[0])
                    .take_while(|(word, first)| word == first)
                    .count()
                    .min(words.len() - 1)
            })
            .min()
            .unwrap_or_default();
        if members.len() == 1 || shared == 0 {
            lines.extend(
                members
                    .iter()
                    .map(|words| format!("- {namespace}{}", words.join("_"))),
            );
        } else {
            factored = true;
            let tails = members
                .iter()
                .map(|words| words[shared..].join("_"))
                .collect::<Vec<_>>()
                .join(",");
            lines.push(format!(
                "- {namespace}{}_{{{tails}}}",
                members[0][..shared].join("_")
            ));
        }
    }
    (lines, factored)
}

#[cfg(test)]
#[path = "roster_tests.rs"]
mod tests;
