//! Human transcript facts. These are never inserted into model-visible history.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModeItem {
    pub id: String,
    /// Full runtime identity, including its session generation.
    pub cell_id: String,
    /// Original exec invocation: durable within the thread even if a runtime restarts.
    pub exec_call_id: String,
    #[ts(type = "number")]
    pub created_at: i64,
    pub event: CodeModeEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[ts(export_to = "v2/")]
pub enum CodeModeEvent {
    Started { call_id: String },
    ToolCall { call_id: String },
    WaitStarted { call_id: String },
    WaitCompleted { call_id: String },
    Finished { status: CodeModeStatus },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum CodeModeStatus {
    Completed,
    Failed,
    Interrupted,
}
