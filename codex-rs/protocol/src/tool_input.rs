//! Human-visible capture of a model's invocation, before dispatch rewrites its arguments.
//! This receipt is never added to the model's history.

use crate::ToolName;
use crate::models::ResponseItem;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ToolInput {
    pub id: String,
    pub call_id: String,
    pub name: ToolName,
    pub payload: ToolInputPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(tag = "format", content = "text", rename_all = "camelCase")]
#[ts(
    tag = "format",
    content = "text",
    rename_all = "camelCase",
    export_to = "v2/"
)]
pub enum ToolInputPayload {
    Json(String),
    Text(String),
}

impl ToolInputPayload {
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Json(text) | Self::Text(text) => text,
        }
    }
}

impl ToolInput {
    #[must_use]
    pub fn capture(item: &ResponseItem) -> Option<Self> {
        let (call_id, name, payload) = match item {
            ResponseItem::FunctionCall {
                call_id,
                name,
                namespace,
                arguments,
                ..
            } => (
                call_id,
                ToolName::new(namespace.clone(), name),
                ToolInputPayload::Json(arguments.clone()),
            ),
            ResponseItem::CustomToolCall {
                call_id,
                name,
                namespace,
                input,
                ..
            } => (
                call_id,
                ToolName::new(namespace.clone(), name),
                ToolInputPayload::Text(input.clone()),
            ),
            _ => return None,
        };
        Some(Self {
            id: format!("input:{call_id}"),
            call_id: call_id.clone(),
            name,
            payload,
        })
    }
}
