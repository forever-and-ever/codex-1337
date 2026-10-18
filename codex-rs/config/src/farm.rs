use codex_protocol::openai_models::ReasoningEffort;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct FarmConfig {
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Executor overrides keyed by the parent thread's configured provider ID.
    pub providers: BTreeMap<String, FarmExecutorConfig>,
    /// Appended to inherited developer instructions, never used for ordinary spawns.
    pub instructions: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct FarmExecutorConfig {
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl FarmConfig {
    #[must_use]
    pub fn executor(&self, provider_id: &str) -> FarmExecutorConfig {
        let provider = self.providers.get(provider_id);
        FarmExecutorConfig {
            model: provider
                .and_then(|entry| entry.model.as_ref())
                .or(self.model.as_ref())
                .cloned(),
            reasoning_effort: provider
                .and_then(|entry| entry.reasoning_effort.clone())
                .or_else(|| self.reasoning_effort.clone()),
        }
    }
}
