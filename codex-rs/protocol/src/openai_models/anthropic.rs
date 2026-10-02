use super::ReasoningEffort;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::num::NonZeroU32;
use ts_rs::TS;

/// Native request capabilities retained with the canonical model identity and cache entry.
/// Context limits and signed compaction support remain on `ModelInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AnthropicModelCapabilities {
    pub thinking: AnthropicThinkingMode,
    #[ts(type = "number")]
    pub max_output_tokens: NonZeroU32,
    pub supported_efforts: Vec<ReasoningEffort>,
    pub supports_mid_conversation_system: bool,
    pub supports_structured_outputs: bool,
}

impl AnthropicModelCapabilities {
    /// Codex's manual effort policy, not an upstream-advertised budget.
    /// A request with a lower output cap must also check that its budget fits.
    pub fn manual_thinking_budget(&self, effort: &ReasoningEffort) -> Option<NonZeroU32> {
        if self.thinking != AnthropicThinkingMode::Manual {
            return None;
        }
        let budget = match effort {
            ReasoningEffort::Minimal | ReasoningEffort::Low => 1024,
            ReasoningEffort::Medium => 8192,
            ReasoningEffort::High => 24576,
            ReasoningEffort::XHigh => 32768,
            ReasoningEffort::Max => 128000,
            ReasoningEffort::None
            | ReasoningEffort::Ultra
            | ReasoningEffort::Persistent
            | ReasoningEffort::Custom(_) => return None,
        };
        NonZeroU32::new(budget).filter(|budget| budget.get() < self.max_output_tokens.get())
    }
}

/// Preferred supported thinking strategy; adaptive wins when both types are advertised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicThinkingMode {
    Disabled,
    Manual,
    Adaptive,
}
