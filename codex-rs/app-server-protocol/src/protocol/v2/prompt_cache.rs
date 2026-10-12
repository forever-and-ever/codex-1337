//! Wire contracts for prompt-cache estimates and warming.

use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

/// Whether the thread's provider prompt cache is confidently warm.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum PromptCacheEstimate {
    /// Observed cache usage with established retention; clients treat it as cold from
    /// `expiresAt` on.
    Warm {
        #[serde(rename = "expiresAt")]
        #[ts(rename = "expiresAt", type = "number")]
        expires_at: i64,
    },
    /// Not confidently warm, including absent evidence or unestablished retention.
    /// This is not a claim that the provider evicted its cache.
    Cold,
}

/// Whether the server is keeping a thread's prompt cache warm.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum PromptCacheWarming {
    Off,
    /// Enabled, waiting for a request that can be replayed; lapses at `endsAt`.
    Armed {
        #[serde(rename = "endsAt")]
        #[ts(rename = "endsAt", type = "number")]
        ends_at: i64,
    },
    /// Replaying the last request to keep its cache warm until `endsAt`.
    Holding {
        #[serde(rename = "endsAt")]
        #[ts(rename = "endsAt", type = "number")]
        ends_at: i64,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadPromptCacheUpdatedNotification {
    pub thread_id: String,
    pub estimate: PromptCacheEstimate,
    pub warming: PromptCacheWarming,
}

/// A request that was expected to read the cache rewrote its prefix instead.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadPromptCacheMissedNotification {
    pub thread_id: String,
    #[ts(type = "number")]
    pub rewritten_tokens: i64,
    /// The miss came from a warming replay, which therefore stopped.
    pub while_warming: bool,
}

/// Warming stopped before its hold ended.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadWarmingStoppedNotification {
    pub thread_id: String,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadSetWarmingParams {
    pub thread_id: String,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadSetWarmingResponse {
    pub estimate: PromptCacheEstimate,
    pub warming: PromptCacheWarming,
}
