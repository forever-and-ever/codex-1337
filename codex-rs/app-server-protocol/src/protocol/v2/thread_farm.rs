use super::UserInput;
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum ThreadFarmMode {
    FullHistory,
    Fresh,
}

/// Direct user delegation, without starting or steering the parent conversation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadFarmParams {
    pub thread_id: String,
    /// UUID identifying this submission across reconnects. Reuse only with identical input/mode.
    pub request_id: String,
    pub mode: ThreadFarmMode,
    pub input: Vec<UserInput>,
    /// Optional literal child-name stem. Numeric suffixes disambiguate occupied names.
    #[ts(optional = nullable)]
    pub task_name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadFarmResponse {
    pub child_thread_id: String,
    pub agent_path: String,
}
