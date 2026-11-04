//! The human-only request bill, without the enclosing item's lifecycle metadata.

use crate::JsonSchema;
use crate::TS;
use codex_protocol::step_admission::Admission;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StepAdmissionItem {
    pub id: String,
    pub response_id: String,
    #[ts(type = "number")]
    pub read_tokens: u64,
    #[ts(type = "number | null")]
    pub thinking_tokens: Option<u64>,
    #[ts(type = "number")]
    pub output_tokens: u64,
    pub call_ids: Vec<String>,
    #[serde(default)]
    pub message_ids: Vec<String>,
    pub admission: Admission,
}

impl From<codex_protocol::step_admission::StepAdmissionItem> for StepAdmissionItem {
    fn from(item: codex_protocol::step_admission::StepAdmissionItem) -> Self {
        Self {
            id: item.id,
            response_id: item.response_id,
            read_tokens: item.read_tokens,
            thinking_tokens: item.thinking_tokens,
            output_tokens: item.output_tokens,
            call_ids: item.call_ids,
            message_ids: item.message_ids,
            admission: item.admission,
        }
    }
}
