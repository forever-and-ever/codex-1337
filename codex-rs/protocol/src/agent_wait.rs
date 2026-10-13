//! Human-only evidence of why an agent wait returned; lifecycle status remains authoritative.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum AgentWaitOutcome {
    TimedOut,
    MailboxActivity,
    Steered,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct AgentWaitCompletion {
    pub outcome: AgentWaitOutcome,
    #[ts(type = "number")]
    pub elapsed_ms: u64,
}
