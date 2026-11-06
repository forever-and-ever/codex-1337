//! On-demand reads of a process's in-memory flight recorder, the only routine log store.

use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export_to = "v2/")]
pub enum FlightRecorderLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Every present criterion must hold; `limit` keeps the newest matches.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct FlightRecorderReadParams {
    #[ts(optional = nullable)]
    pub thread_id: Option<String>,
    /// Least severe level admitted.
    #[ts(optional = nullable)]
    pub min_level: Option<FlightRecorderLevel>,
    #[ts(optional = nullable)]
    pub target_prefix: Option<String>,
    #[ts(optional = nullable, type = "number | null")]
    pub since_unix_ms: Option<i64>,
    #[ts(optional = nullable, type = "number | null")]
    pub until_unix_ms: Option<i64>,
    /// Only records after this `seq`; a follower passes the last one it printed.
    #[ts(optional = nullable, type = "number | null")]
    pub after_seq: Option<u64>,
    /// Newest matching records returned; 2000 when absent.
    #[ts(optional = nullable)]
    pub limit: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct FlightRecorderReadResponse {
    /// Oldest first.
    pub records: Vec<FlightRecorderRecord>,
    /// Occupancy per retention tier; a tier's horizon bounds what any query can see.
    pub tiers: Vec<FlightRecorderTier>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct FlightRecorderRecord {
    /// Increases by push order within one process.
    #[ts(type = "number")]
    pub seq: u64,
    #[ts(type = "number")]
    pub unix_ms: i64,
    pub level: FlightRecorderLevel,
    pub target: String,
    pub thread_id: Option<String>,
    /// Span chain, message and fields.
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export_to = "v2/")]
pub enum FlightRecorderTierName {
    /// WARN and ERROR.
    Alert,
    Info,
    /// DEBUG and TRACE.
    Chatter,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct FlightRecorderTier {
    pub tier: FlightRecorderTierName,
    #[ts(type = "number")]
    pub budget_bytes: u64,
    #[ts(type = "number")]
    pub used_bytes: u64,
    #[ts(type = "number | null")]
    pub oldest_unix_ms: Option<i64>,
}
