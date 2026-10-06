//! Canonical model metadata for the subscription provider's Codex tool path.

use codex_protocol::openai_models::ApplyPatchToolType;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::MultiAgentVersion;

pub(crate) fn normalize(models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    models
        .into_iter()
        .filter(|model| model.slug.starts_with("claude-"))
        .map(normalize_model_info)
        .collect()
}

pub(crate) fn normalize_model_info(mut model: ModelInfo) -> ModelInfo {
    // Keep the established Code Mode surface while projecting native client tools.
    model.tool_mode = Some(ToolMode::CodeModeOnly);
    model.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    model.multi_agent_version = Some(MultiAgentVersion::V2);
    model.additional_speed_tiers.clear();
    model.service_tiers.clear();
    model.default_service_tier = None;
    model.supported_in_api = true;
    if model.used_fallback_model_metadata {
        // Missing metadata cannot establish a larger model's context entitlement.
        model.context_window = Some(200_000);
        model.max_context_window = Some(200_000);
    }
    model
}

#[cfg(test)]
#[path = "claude_catalog_tests.rs"]
mod tests;
