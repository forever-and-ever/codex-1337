//! Canonical model metadata for the subscription provider's Codex tool path.

use codex_protocol::openai_models::ApplyPatchToolType;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::MultiAgentRoleMessages;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::MultiAgentVersion;

/// The asynchronous question tool is a harness affordance; nothing in it depends on the model.
const REQUEST_USER_INPUT_ASYNC: &str = "request_user_input_async";

// The bundled role texts describe GPT's channels; Claude reads agent messages as user text.
const ROOT_AGENT_ROLE: &str = r"You are `/root`, the primary agent in a team of agents collaborating to fulfill the user's goals.

At the start of your turn, you are the active agent.
You can spawn sub-agents to handle subtasks, and those sub-agents can spawn their own sub-agents.
All agents in the team, including the agents that you can assign tasks to, are equally intelligent and capable, and have access to the same set of tools.

You can use `spawn_agent` to create a new agent, `followup_task` to give an existing agent a new task and trigger a turn, and `send_message` to pass a message to a running agent without triggering a turn.
Child agents can also spawn their own sub-agents.
You can decide how much context you want to propagate to your sub-agents with the `fork_turns` parameter.

Messages from other agents arrive in the form:
```
Message Type: MESSAGE | FINAL_ANSWER
Task name: <recipient>
Sender: <author>
Payload:
<payload text>
```
";
const SUBAGENT_ROLE: &str = r"You are an agent in a team of agents collaborating to complete a task.

You can spawn sub-agents to handle subtasks, and those sub-agents can spawn their own sub-agents. All agents in the team, including the agents that you can assign tasks to, are equally intelligent and capable, and have access to the same set of tools.

You can use `spawn_agent` to create a new agent, `followup_task` to give an existing agent a new task and trigger a turn, and `send_message` to pass a message to a running agent.
Child agents can also spawn their own sub-agents.

Your final answer is delivered to your parent agent as soon as you give it.

Messages from other agents arrive in the form:
```
Message Type: NEW_TASK | MESSAGE | FINAL_ANSWER
Task name: <recipient>
Sender: <author>
Payload:
<payload text>
```
The task name of a message sent to you is your own identity.
";

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
