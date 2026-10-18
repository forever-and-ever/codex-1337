use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

/// A rejected provider-native invocation, retained for exact tool-result pairing.
/// This is evidence of an attempted call, never authority to dispatch a local tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum NativeToolCall {
    Anthropic {
        call_id: String,
        name: String,
        #[ts(type = "unknown")]
        input: Value,
    },
}

impl NativeToolCall {
    #[must_use]
    pub fn call_id(&self) -> &str {
        match self {
            Self::Anthropic { call_id, .. } => call_id,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Anthropic { name, .. } => name,
        }
    }
}
