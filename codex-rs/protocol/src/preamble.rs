//! The initial context of one window, recorded for inspection and never model input.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Initial instructions, tools and managed context of a context window.
/// Completed with that window's first successful response; later arrivals are Injections.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
pub struct PreambleItem {
    pub id: String,
    pub response_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    pub completed_at_ms: i64,
    pub parts: Vec<PreamblePart>,
    /// Exact request-side special material. Missing only in older rollout records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// Legacy rolling-census delta. New per-window preambles always leave this absent.
    pub change: Option<PreambleChange>,
    /// Provider-reported input of the whole request, conversation included.
    #[ts(type = "number | null")]
    pub reported_input_tokens: Option<u64>,
}

/// Retained special material, including superseded instructions and their amendments.
/// An inventory is not an arrival and is never inserted into model history.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ContextInventory {
    pub parts: Vec<PreamblePart>,
    pub raw: String,
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
