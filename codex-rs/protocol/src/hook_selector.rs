use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Selects a hook's tool address or its lifecycle event value, never a flattened name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HookSelector {
    /// Omitted components match any value. An explicit tool selector requires an attributed tool.
    Tool {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Matches the event's source, reason, trigger, or agent type, not its event name.
    EventValue { pattern: String },
}
