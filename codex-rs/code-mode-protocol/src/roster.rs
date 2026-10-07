//! The roster names the nested tools whose declarations `exec`'s description withholds.

use crate::ToolDefinition;
use crate::normalize_code_mode_identifier;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

const HEADER: &str = "These nested tools are callable on the global `tools` object but not declared in this description; read a tool's contract with `describe_tool` before its first use. `a_{b,c}` stands for `a_b` and `a_c`.";

/// A nested tool that `exec`'s description does not declare.
#[derive(Clone, Debug, PartialEq)]
pub enum UndeclaredTool {
    /// Named in the roster; `describe_tool` supplies its contract.
    Rostered(ToolDefinition),
    /// Neither declared nor named; search finds it.
    Deferred(ToolDefinition),
}

impl UndeclaredTool {
    pub(crate) fn definition(&self) -> &ToolDefinition {
        match self {
            Self::Rostered(definition) | Self::Deferred(definition) => definition,
        }
    }
}

/// Lists each rostered tool's callable name once. Names sharing a namespace and leading words
/// are factored into one line, so a large MCP server costs a few lines instead of its schemas.
pub(crate) fn render_roster(tools: &[UndeclaredTool]) -> Option<String> {
    let names = tools
        .iter()
        .filter_map(|tool| match tool {
            UndeclaredTool::Rostered(definition) => {
                Some(normalize_code_mode_identifier(&definition.name))
            }
            UndeclaredTool::Deferred(_) => None,
        })
        .collect::<BTreeSet<_>>();
    if names.is_empty() {
        return None;
    }
    let mut families = BTreeMap::<(&str, &str), Vec<Vec<&str>>>::new();
    for name in &names {
        let (namespace, rest) = name
            .rfind("__")
            .map_or(("", name.as_str()), |at| name.split_at(at + 2));
        let words = rest.split('_').collect::<Vec<_>>();
        families
            .entry((namespace, words[0]))
            .or_default()
            .push(words);
    }
    let mut lines = vec![HEADER.to_string()];
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
    Some(lines.join("\n"))
}

#[cfg(test)]
#[path = "roster_tests.rs"]
mod tests;
