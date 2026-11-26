//! Deferral withholds a contract, never a tool's existence or purpose.
//! The roster is context the model keeps: a catalogue change amends it instead of redeclaring
//! `exec`.
use crate::ToolDefinition;
use crate::discovery::compact_tool_summary;
use crate::presentation::ToolDialect;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The markers of the roster and of every amendment to it.
pub const ROSTER_TAGS: (&str, &str) = ("<nested_tools>", "</nested_tools>");
const HEADER: &str = "Nested tools, each called inside `exec` by its `tools` member:";

#[derive(Clone, Debug, PartialEq)]
pub struct UndeclaredTool(pub ToolDefinition);

impl UndeclaredTool {
    pub(crate) fn definition(&self) -> &ToolDefinition {
        &self.0
    }
}

/// One nested tool as the roster lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterEntry {
    /// The `tools` member, after the provider's reference where the two differ.
    pub name: String,
    /// One purpose sentence.
    pub purpose: String,
}

/// The purpose the model last read for each listed name.
pub type RosterRecord = BTreeMap<String, String>;

/// Every nested tool, ordered by name.
pub fn roster(tools: &[UndeclaredTool], dialect: ToolDialect) -> Vec<RosterEntry> {
    tools
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
        .collect::<RosterRecord>()
        .into_iter()
        .map(|(name, purpose)| RosterEntry { name, purpose })
        .collect()
}

pub fn roster_record(entries: &[RosterEntry]) -> RosterRecord {
    entries
        .iter()
        .map(|entry| (entry.name.clone(), entry.purpose.clone()))
        .collect()
}

/// The whole roster, as a window first states it. An empty roster says nothing.
pub fn render_roster(entries: &[RosterEntry]) -> Option<String> {
    (!entries.is_empty()).then(|| {
        let mut body = format!("\n{HEADER}\n");
        for entry in entries {
            let _ = writeln!(body, "- {}: {}", entry.name, entry.purpose);
        }
        body
    })
}

/// What changed since the roster the model last read; nothing when the two agree.
pub fn render_roster_amendment(entries: &RosterRecord, told: &RosterRecord) -> Option<String> {
    if entries.is_empty() {
        return (!told.is_empty()).then(|| "\nNo rostered tools remain.\n".to_owned());
    }
    let mut body = String::from("\n");
    let mut removed = String::new();
    let mut previous = told.iter().peekable();
    for (name, purpose) in entries {
        while let Some((name, _)) = previous.next_if(|(prior, _)| *prior < name) {
            let _ = writeln!(removed, "- {name}");
        }
        if previous
            .next_if(|(prior, _)| *prior == name)
            .is_some_and(|(_, previous_purpose)| previous_purpose == purpose)
        {
            continue;
        }
        if body.len() == 1 {
            body.push_str("Added or changed:\n");
        }
        let _ = writeln!(body, "- {name}: {purpose}");
    }
    for (name, _) in previous {
        let _ = writeln!(removed, "- {name}");
    }
    if !removed.is_empty() {
        body.push_str("Removed:\n");
        body.push_str(&removed);
    }
    (body.len() > 1).then_some(body)
}

#[cfg(test)]
#[path = "roster_tests.rs"]
mod tests;
