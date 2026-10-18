use codex_protocol::openai_models::AnthropicCacheTtl;
use codex_protocol::openai_models::ReasoningEffort;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnthropicPromptCache {
    #[serde(default = "default_ttl")]
    ttl: AnthropicCacheTtl,
    #[serde(default)]
    rules: Vec<CacheRule>,
}

fn default_ttl() -> AnthropicCacheTtl {
    AnthropicCacheTtl::OneHour
}

impl Default for AnthropicPromptCache {
    fn default() -> Self {
        Self {
            ttl: default_ttl(),
            rules: Vec::new(),
        }
    }
}

impl AnthropicPromptCache {
    pub fn resolve(
        &self,
        model: &str,
        effective_effort: Option<&ReasoningEffort>,
    ) -> AnthropicCacheTtl {
        self.rules
            .iter()
            .find(|rule| {
                model.starts_with(&rule.model.0)
                    && rule.efforts.as_ref().is_none_or(|efforts| {
                        effective_effort.is_some_and(|effort| efforts.0.contains(effort))
                    })
            })
            .map_or(self.ttl, |rule| rule.ttl)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CacheRule {
    model: ModelPrefix,
    efforts: Option<Efforts>,
    ttl: AnthropicCacheTtl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
struct ModelPrefix(#[schemars(length(min = 1))] String);

impl TryFrom<String> for ModelPrefix {
    type Error = &'static str;

    fn try_from(prefix: String) -> Result<Self, Self::Error> {
        if prefix.is_empty() {
            return Err("Anthropic prompt-cache model prefix must not be empty");
        }
        Ok(Self(prefix))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "Vec<ReasoningEffort>")]
struct Efforts(#[schemars(length(min = 1))] Vec<ReasoningEffort>);

impl TryFrom<Vec<ReasoningEffort>> for Efforts {
    type Error = &'static str;

    fn try_from(efforts: Vec<ReasoningEffort>) -> Result<Self, Self::Error> {
        if efforts.is_empty() {
            return Err("Anthropic prompt-cache efforts must not be empty");
        }
        Ok(Self(efforts))
    }
}

#[cfg(test)]
#[path = "anthropic_prompt_cache_tests.rs"]
mod tests;
