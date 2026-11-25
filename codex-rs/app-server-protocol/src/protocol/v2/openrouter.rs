use super::Model;
use crate::JsonSchema;
use crate::TS;
use codex_protocol::EndpointFilter;
use codex_protocol::EndpointSlug;
use codex_protocol::KeyName;
use codex_protocol::ModelSlug;
use codex_protocol::Usd;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterKey {
    pub name: KeyName,
    pub path: AbsolutePathBuf,
    pub is_default: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterKeyStatus {
    /// Locally admitted jobs whose invoice is not yet known, including ambiguous submissions.
    #[serde(default)]
    pub outstanding_batches: u32,
    pub key: OpenRouterKey,
    pub state: OpenRouterKeyState,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export_to = "v2/")]
pub enum OpenRouterKeyState {
    Read {
        label: String,
        balance: Option<Usd>,
        limit: Option<OpenRouterKeyLimit>,
        spend: OpenRouterSpend,
        #[serde(rename = "expiresAt")]
        #[ts(type = "number | null", rename = "expiresAt")]
        expires_at: Option<i64>,
        #[serde(rename = "capturedAt")]
        #[ts(type = "number", rename = "capturedAt")]
        captured_at: i64,
    },
    Failed {
        error: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterKeyLimit {
    pub limit: Usd,
    pub remaining: Usd,
    pub reset: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterSpend {
    pub daily: Usd,
    pub weekly: Usd,
    pub monthly: Usd,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterHost {
    pub slug: EndpointSlug,
    pub name: String,
    pub endpoints: Vec<OpenRouterEndpoint>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterEndpoint {
    pub slug: EndpointSlug,
    pub quantization: Option<String>,
    pub service_tier: Option<String>,
    #[ts(type = "number")]
    pub context_length: i64,
    pub prompt_per_mtok: Option<Usd>,
    pub completion_per_mtok: Option<Usd>,
    pub uptime_percent: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterKeyAddParams {
    pub name: KeyName,
    pub path: AbsolutePathBuf,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterKeyAddResponse {
    pub key: OpenRouterKey,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterKeyRemoveParams {
    pub name: KeyName,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[ts(export_to = "v2/")]
pub struct OpenRouterKeyRemoveResponse {}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterKeyStatusParams {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub name: Option<KeyName>,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterKeyStatusResponse {
    pub keys: Vec<OpenRouterKeyStatus>,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterModelSaveParams {
    pub slug: ModelSlug,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterModelSaveResponse {
    pub model: Model,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterModelForgetParams {
    pub slug: ModelSlug,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[ts(export_to = "v2/")]
pub struct OpenRouterModelForgetResponse {}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterModelEndpointsParams {
    pub slug: ModelSlug,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterModelEndpointsResponse {
    pub hosts: Vec<OpenRouterHost>,
    pub filter: EndpointFilter,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct OpenRouterModelRouteParams {
    pub slug: ModelSlug,
    pub filter: EndpointFilter,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[ts(export_to = "v2/")]
pub struct OpenRouterModelRouteResponse {}
