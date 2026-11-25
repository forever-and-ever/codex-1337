use super::AnthropicOperation;
use super::AnthropicPrompt;
use super::AnthropicRequest;
use super::AnthropicRequestOptions;
use super::cache::mark_prefixes;
use super::history::encode_history;
use super::history::invalid;
use super::history::separate;
use crate::ApiError;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::AnthropicThinkingMode;
use codex_protocol::openai_models::ReasoningEffort;
use serde_json::json;
use std::sync::Arc;

// The subscription endpoint requires this fixed leading block. Sonnet rejects
// otherwise identical OAuth requests without it. Keep it at the wire boundary:
// the caller's authority blocks and canonical history remain intact after it.
const SUBSCRIPTION_PREAMBLE: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Corrects the identity the required block asserts, before any instruction relies on it.
// TELOS(claude-native-voice): the second block names the model and the harness it runs in.
fn subscription_harness(model: &str) -> String {
    format!(
        "That line is a subscription formality. You are `{model}`, running in Codex-1337, a fork of OpenAI's Codex CLI, not Claude Code: project instructions come from AGENTS.md, and Claude Code's tools and commands do not exist here."
    )
}

/// Encode canonical agent history as an Anthropic subscription Messages request.
pub fn encode_anthropic_request(
    prompt: &AnthropicPrompt<'_>,
    options: &AnthropicRequestOptions<'_>,
) -> Result<AnthropicRequest, ApiError> {
    let capabilities = options
        .model
        .anthropic
        .as_ref()
        .ok_or_else(|| invalid("selected model has no native Anthropic capabilities"))?;
    if options.max_output_tokens > capabilities.max_output_tokens {
        return Err(invalid(
            "requested Anthropic output exceeds the declared model limit",
        ));
    }
    let replays_compaction = prompt
        .input
        .iter()
        .any(|item| matches!(item, ResponseItem::NativeCompaction { .. }));
    if options.operation == AnthropicOperation::Compact && !options.model.supports_native_compaction
    {
        return Err(invalid(
            "selected Anthropic model does not support signed compaction",
        ));
    }
    if !options.model.supports_native_compaction && replays_compaction {
        return Err(invalid(
            "selected Anthropic model cannot replay signed compaction",
        ));
    }
    let mut history = encode_history(
        prompt.input,
        prompt.instructions,
        prompt.tools,
        &options.model.input_modalities,
    )?;
    history.system.splice(
        0..0,
        [
            SUBSCRIPTION_PREAMBLE.to_owned(),
            subscription_harness(&options.model.slug),
        ]
        .map(|text| json!({"type":"text", "text":text})),
    );
    separate(&mut history.system);
    let mut tools = prompt
        .tools
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| invalid(format!("invalid Anthropic tool declaration: {err}")))?;
    mark_prefixes(&mut history, &mut tools, options.cache);
    let mut body = json!({
        "model":options.model.slug,
        "max_tokens":options.max_output_tokens,
        "stream":true,
        "messages":history.messages,
    });
    if !history.system.is_empty() {
        body["system"] = json!(history.system);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["tool_choice"] =
            json!({"type":"auto", "disable_parallel_tool_use":!prompt.parallel_tool_calls});
    }
    let mut betas = Vec::new();
    if options.operation == AnthropicOperation::Compact || replays_compaction {
        betas.push("compact-2026-09-04");
    }
    let effort = options
        .reasoning_effort
        .or(options.model.default_reasoning_level.as_ref());
    if let Some(effort) = effort {
        if !options
            .model
            .supported_reasoning_levels
            .iter()
            .any(|preset| &preset.effort == effort)
        {
            return Err(invalid(format!(
                "selected Anthropic model does not declare reasoning effort {}",
                effort.as_str()
            )));
        }
        match (capabilities.thinking, effort) {
            (_, ReasoningEffort::None) => body["thinking"] = json!({"type":"disabled"}),
            (AnthropicThinkingMode::Adaptive, effort) => {
                if !capabilities.supported_efforts.contains(effort) {
                    return Err(invalid(
                        "reasoning effort is not an advertised Anthropic output effort",
                    ));
                }
                body["thinking"] = json!({"type":"adaptive"});
                body["output_config"]["effort"] = json!(effort.as_str());
            }
            (AnthropicThinkingMode::Manual, effort) => {
                let budget = capabilities.manual_thinking_budget(effort).ok_or_else(|| {
                    invalid("Anthropic reasoning effort has no supported thinking budget")
                })?;
                if budget >= options.max_output_tokens {
                    return Err(invalid(
                        "Anthropic thinking budget must be less than max output tokens",
                    ));
                }
                body["thinking"] = json!({"type":"enabled", "budget_tokens":budget});
            }
            (AnthropicThinkingMode::Disabled, _) => {
                return Err(invalid(
                    "selected Anthropic model does not support thinking",
                ));
            }
        }
    } else if capabilities.thinking == AnthropicThinkingMode::Adaptive {
        body["thinking"] = json!({"type":"adaptive"});
    }
    if let Some(schema) = prompt.output_schema {
        if options.operation == AnthropicOperation::Compact {
            return Err(invalid(
                "Anthropic signed compaction cannot request structured output",
            ));
        }
        if !capabilities.supports_structured_outputs {
            return Err(invalid(
                "selected Anthropic model does not support structured output",
            ));
        }
        body["output_config"]["format"] = json!({"type":"json_schema", "schema":schema});
    }
    if options.operation == AnthropicOperation::Compact {
        body["compaction"] = json!({"type":"summarize"});
    }
    AnthropicRequest::from_body(&body, Arc::clone(prompt.tools), Arc::from(betas))
}
