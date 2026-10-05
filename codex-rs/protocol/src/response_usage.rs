//! Per-response usage metadata reported by the upstream service, without aggregation.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

/// Usage metadata reported for one upstream response.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
pub struct ResponseUsageMetadata {
    pub amount: Option<String>,
    pub metadata: Option<Value>,
    /// Provider-reported reasoning attribution within the inclusive output count.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub reported_reasoning_tokens: Option<u64>,
}

/// Human-transcript data for one completed inference, never model input.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
pub struct ResponseThought {
    pub turn_id: String,
    pub completed_at_ms: i64,
    pub summary: Vec<String>,
    pub content: Vec<String>,
}
