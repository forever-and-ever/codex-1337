//! Canonical model metadata for the subscription provider's Codex tool path.

use codex_protocol::openai_models::ApplyPatchToolType;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::MultiAgentRoleMessages;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::openai_models::TruncationPolicyConfig;
use codex_protocol::protocol::MultiAgentVersion;

/// The asynchronous question tool is a harness affordance; nothing in it depends on the model.
const REQUEST_USER_INPUT_ASYNC: &str = "request_user_input_async";

// The bundled role texts describe GPT's channels; native Claude reads each agent message in the
// envelope the Anthropic adapter gives it.
const ROOT_AGENT_ROLE: &str = r#"You are `/root`, the primary agent of a team working on the user's goals.

`spawn_agent` creates a sub-agent, `followup_task` gives an existing agent a new task and starts its turn, and `send_message` passes a message to a running agent without starting a turn. `fork_turns` sets how much of your context a new agent inherits. Sub-agents can spawn their own.

A message from another agent arrives as `<agent_message from="..." to="..." type="message|final_answer">`: `from` is its author and `to` its recipient.
"#;
const SUBAGENT_ROLE: &str = r#"You are an agent in a team working on a task.

`spawn_agent` creates a sub-agent, `followup_task` gives an existing agent a new task and starts its turn, and `send_message` passes a message to a running agent without starting a turn. Sub-agents can spawn their own.

Your final answer goes to your parent agent as soon as you give it.

A message from another agent arrives as `<agent_message from="..." to="..." type="new_task|message|final_answer">`: `from` is its author, and `to` is your own path in the team.
"#;

pub(crate) fn normalize(models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    models
        .into_iter()
        .filter(|model| model.slug.starts_with("claude-"))
        .map(|model| normalize_model_info(model, codex_models_manager::ModelOrigin::Provider))
        .collect()
}

pub(crate) fn normalize_model_info(
    mut model: ModelInfo,
    origin: codex_models_manager::ModelOrigin,
) -> ModelInfo {
    // Keep the established Code Mode surface while projecting native client tools.
    model.tool_mode = Some(ToolMode::CodeModeOnly);
    model.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    model.multi_agent_version = Some(MultiAgentVersion::V2);
    model.additional_speed_tiers.clear();
    model.service_tiers.clear();
    model.default_service_tier = None;
    model.supported_in_api = true;
    // The tool descriptions promise this default; the catalogue's byte fallback is a quarter of it.
    model.truncation_policy = TruncationPolicyConfig::tokens(/*limit*/ 10_000);
    if !model
        .experimental_supported_tools
        .iter()
        .any(|tool| tool == REQUEST_USER_INPUT_ASYNC)
    {
        model
            .experimental_supported_tools
            .push(REQUEST_USER_INPUT_ASYNC.to_string());
    }
    model
        .model_messages
        .get_or_insert_default()
        .multi_agent
        .get_or_insert_default()
        .role = Some(MultiAgentRoleMessages {
        root: Some(ROOT_AGENT_ROLE.to_string()),
        subagent: Some(SUBAGENT_ROLE.to_string()),
    });
    if origin == codex_models_manager::ModelOrigin::Synthetic {
        // Missing metadata cannot establish a larger model's context entitlement.
        model.context_window = Some(200_000);
        model.max_context_window = Some(200_000);
    }
    model
}

#[cfg(test)]
#[path = "claude_catalog_tests.rs"]
mod tests;
