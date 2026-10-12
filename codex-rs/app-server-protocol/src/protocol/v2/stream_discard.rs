use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StreamItemsDiscardedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_ids: Vec<String>,
}
