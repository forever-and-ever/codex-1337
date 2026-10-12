use crate::ThreadId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Unfinished display items abandoned before another sampling attempt.
/// Completed response items and tool executions are never listed.
#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct StreamItemsDiscardedEvent {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub item_ids: Vec<String>,
}
