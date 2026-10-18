use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::EnumString;
use ts_rs::TS;

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS, EnumString,
)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
#[strum(serialize_all = "lowercase")]
pub enum CommsGate {
    #[default]
    Open,
    Closed,
    Blocked,
}

impl CommsGate {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Blocked => "blocked",
        }
    }
}
