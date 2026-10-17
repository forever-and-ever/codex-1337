use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ThreadForkSnapshot {
    #[default]
    Interrupted,
    /// Retain an unfinished prefix without telling the model that its source was interrupted.
    Side,
}

impl ThreadForkSnapshot {
    pub(super) fn is_default(&self) -> bool {
        *self == Self::Interrupted
    }
}
