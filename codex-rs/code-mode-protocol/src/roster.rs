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

pub(crate) fn render_roster_for(tools: &[UndeclaredTool], dialect: ToolDialect) -> Option<String> {
    let entries: BTreeMap<_, _> = tools
        .iter()
        .map(|tool| {
            let definition = tool.definition();
            let purpose = if definition.summary.is_empty() {
                &definition.description
            } else {
                &definition.summary
            };
            (dialect.heading(definition), compact_tool_summary(purpose))
        })
        .collect();
    (!entries.is_empty()).then(|| {
        std::iter::once(HEADER.to_string())
            .chain(
                entries
                    .into_iter()
                    .map(|(name, summary)| format!("- {name}: {summary}")),
            )
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[cfg(test)]
#[path = "roster_tests.rs"]
mod tests;
