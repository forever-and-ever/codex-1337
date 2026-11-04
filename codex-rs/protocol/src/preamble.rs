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
    pub parts: Vec<PreamblePart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<_>")]
    pub sources: Vec<ContextSource>,
    /// Legacy mixed capture. New records preserve exact text in typed sources.
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
    /// Legacy mixed capture; new records use typed sources.
    #[serde(default)]
    pub raw: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<_>")]
    pub sources: Vec<ContextSource>,
}

/// Captured text and a client serialization of declarations are different evidence.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(tag = "kind", rename_all = "camelCase")]
pub enum ContextSource {
    Prompt {
        label: String,
        text: String,
    },
    /// The declarations supplied to the provider, serialized by this client for inspection.
    /// This is not the provider's internal prompt serialization.
    ToolDeclarations {
        text: String,
    },
}

/// Local o200k accounting, or an explicit refusal to spend unbounded census work.
/// Numeric captures from older clients remain valid counted values.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
#[serde(untagged)]
#[ts(untagged)]
pub enum CensusCount {
    Counted(#[ts(type = "number")] u64),
    Omitted(CensusOmission),
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum CensusOmission {
    WorkLimit,
}

impl CensusCount {
    pub fn counted(self) -> Option<u64> {
        match self {
            Self::Counted(tokens) => Some(tokens),
            Self::Omitted(_) => None,
        }
    }
}

impl std::ops::Add for CensusCount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        match (self, rhs) {
            (Self::Counted(left), Self::Counted(right)) => Self::Counted(left + right),
            (Self::Omitted(reason), _) | (_, Self::Omitted(reason)) => Self::Omitted(reason),
        }
    }
}

impl std::iter::Sum for CensusCount {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::Counted(0), std::ops::Add::add)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PreamblePart {
    pub section: PreambleSection,
    /// The MCP server or tool namespace a tool belongs to.
    pub group: Option<String>,
    pub label: String,
    pub tokens: CensusCount,
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
