//! Deferral withholds a contract, never a tool's existence or purpose.
use crate::ToolDefinition;
use crate::discovery::compact_tool_summary;
use crate::presentation::ToolDialect;
use std::collections::BTreeMap;

const HEADER: &str =
    "Available nested tools — read a missing contract with describe_tool before calling.";

#[derive(Clone, Debug, PartialEq)]
pub struct UndeclaredTool(pub ToolDefinition);

impl UndeclaredTool {
    pub(crate) fn definition(&self) -> &ToolDefinition {
        &self.0
    }
}

pub(crate) fn definitions_for(
    tools: &[UndeclaredTool],
    dialect: ToolDialect,
) -> Vec<crate::ExecDefinition> {
    let entries: BTreeMap<_, _> = tools
        .iter()
        .map(|tool| {
            let definition = tool.definition();
            let purpose = if definition.summary.is_empty() {
                &definition.description
            } else {
                &definition.summary
            };
            (
                dialect.heading(definition),
                (
                    crate::normalize_code_mode_identifier(&definition.name),
                    compact_tool_summary(purpose),
                ),
            )
        })
        .collect();
    entries
        .into_iter()
        .map(|(name, (member, summary))| crate::ExecDefinition {
            key: format!("Tool: tools.{member}"),
            description: format!("- {name}: {summary}"),
        })
        .collect()
}

pub(crate) fn render_definitions(entries: &[crate::ExecDefinition]) -> Option<String> {
    (!entries.is_empty()).then(|| {
        std::iter::once(HEADER)
            .chain(entries.iter().map(|entry| entry.description.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[cfg(test)]
pub(crate) fn render_roster_for(tools: &[UndeclaredTool], dialect: ToolDialect) -> Option<String> {
    render_definitions(&definitions_for(tools, dialect))
}

#[cfg(test)]
#[path = "roster_tests.rs"]
mod tests;
