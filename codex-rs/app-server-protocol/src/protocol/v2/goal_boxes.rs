//! Goal box requests (codex-1337): a root goal's timebox and quotabox.

use crate::JsonSchema;
use crate::TS;
use codex_protocol::goal_boxes::GoalBoxes;
use codex_protocol::goal_boxes::QuotaGauge;
use codex_protocol::goal_boxes::Quotabox;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadGoalBoxesReadParams {
    pub thread_id: String,
}

/// Sets the goal's timebox to end at `deadline`, in Unix seconds, or clears it without one.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadGoalTimeboxSetParams {
    pub thread_id: String,
    #[ts(optional = nullable)]
    pub deadline: Option<i64>,
}

/// Sets the goal's quotabox, or clears it without one.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadGoalQuotaboxSetParams {
    pub thread_id: String,
    #[ts(optional = nullable)]
    pub quotabox: Option<Quotabox>,
}

/// The goal's boxes, with the quota its quotabox would watch when the request read it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadGoalBoxesReadResponse {
    pub boxes: GoalBoxes,
    #[ts(optional = nullable)]
    pub gauge: Option<QuotaGauge>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadGoalTimeboxSetResponse {
    pub boxes: GoalBoxes,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadGoalQuotaboxSetResponse {
    pub boxes: GoalBoxes,
    #[ts(optional = nullable)]
    pub gauge: Option<QuotaGauge>,
}
