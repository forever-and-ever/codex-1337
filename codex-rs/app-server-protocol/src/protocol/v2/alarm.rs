//! Thread-owned alarms and their client controls.

use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AlarmSummary {
    pub id: String,
    /// Unix milliseconds.
    #[ts(type = "number")]
    pub armed_at: i64,
    pub note: String,
    pub condition: AlarmCondition,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum AlarmCondition {
    Clock {
        /// Unix milliseconds.
        #[ts(type = "number")]
        at: i64,
    },
    Watch {
        #[serde(rename = "sessionId")]
        #[ts(rename = "sessionId")]
        session_id: i32,
        command: String,
        pattern: Option<String>,
        /// Unix milliseconds.
        #[ts(type = "number | null")]
        deadline: Option<i64>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadAlarmsUpdatedNotification {
    pub thread_id: String,
    pub alarms: Vec<AlarmSummary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadAlarmDisarmParams {
    pub thread_id: String,
    pub id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadAlarmDisarmResponse {
    pub alarms: Vec<AlarmSummary>,
}
