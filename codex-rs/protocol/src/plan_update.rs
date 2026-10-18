//! Checklist progress metadata, separate from the model's `update_plan` arguments.

use crate::plan_tool::UpdatePlanArgs;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PlanUpdateEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub call_id: Option<String>,
    #[serde(flatten)]
    pub update: UpdatePlanArgs,
}

impl From<UpdatePlanArgs> for PlanUpdateEvent {
    fn from(update: UpdatePlanArgs) -> Self {
        Self {
            call_id: None,
            update,
        }
    }
}
