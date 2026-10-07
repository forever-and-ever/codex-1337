//! Census of the standing model context, a human-only record that is never model input.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Local o200k census of the standing part of the model context one request carried:
/// its instructions, tool schemas and injected context. Completed with the response of a
/// runtime's first request and of every request whose instructions or tool set changed.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
pub struct PreambleItem {
    pub id: String,
    pub response_id: String,
    pub completed_at_ms: i64,
    pub parts: Vec<PreamblePart>,
    /// Difference from the runtime's previous census; absent for its first.
    pub change: Option<PreambleChange>,
    /// Provider-reported input of the whole request, conversation included.
    #[ts(type = "number | null")]
    pub reported_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PreamblePart {
    pub section: PreambleSection,
    /// The MCP server or tool namespace a tool belongs to.
    pub group: Option<String>,
    pub label: String,
    #[ts(type = "number")]
    pub tokens: u64,
}

#[derive(
    Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, TS, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum PreambleSection {
    Instructions,
    Tools,
    Context,
}

/// Parts named `group/label`, or `label` when ungrouped.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PreambleChange {
    #[ts(type = "number")]
    pub previous_tokens: u64,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub resized: Vec<String>,
}
