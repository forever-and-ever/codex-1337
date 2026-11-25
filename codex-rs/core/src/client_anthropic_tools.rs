//! Projects the selected tool surface without changing Codex dispatch identities.

use codex_api::AnthropicTool;
use codex_api::AnthropicToolIdentity;
use codex_api::ApiError;
use codex_api::anthropic_function_input_schema;
use codex_tools::FreeformTool;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

pub(crate) fn project_tools(specs: &[ToolSpec]) -> Result<Arc<[AnthropicTool]>, ApiError> {
    let mut tools = Vec::new();
    for spec in specs {
        match spec {
            ToolSpec::Function(tool) => tools.push(function(tool, /*namespace*/ None)?),
            ToolSpec::Freeform(tool) => tools.push(custom(tool, /*namespace*/ None)?),
            ToolSpec::Namespace(namespace) => {
                for (index, tool) in namespace.tools.iter().enumerate() {
                    let mut tool = match tool {
                        ResponsesApiNamespaceTool::Function(tool) => {
                            function(tool, Some(&namespace.name))?
                        }
                        ResponsesApiNamespaceTool::Custom(tool) => {
                            custom(tool, Some(&namespace.name))?
                        }
                    };
                    // The namespace's instructions open its first tool; the model reads every
                    // tool description, so the others need no pointer back to it.
                    if index == 0 && !namespace.description.is_empty() {
                        tool.description =
                            format!("{}\n\n{}", namespace.description, tool.description);
                    }
                    tools.push(tool);
                }
            }
            ToolSpec::ToolSearch { .. } | ToolSpec::WebSearch { .. } => {
                return Err(ApiError::InvalidRequest {
                    message: format!("Anthropic does not support the {} builtin", spec.name()),
                });
            }
        }
    }

    // Dispatch addresses and wire aliases must both be unique. The alias itself
    // depends only on identity, so removing a tool cannot change history replay.
    let mut addresses = HashSet::new();
    let mut resolved = HashSet::new();
    for tool in &tools {
        let (name, namespace) = match &tool.identity {
            AnthropicToolIdentity::Function { name, namespace }
            | AnthropicToolIdentity::Custom { name, namespace } => (name, namespace),
        };
        if !addresses
            .insert(ToolName::new(namespace.clone(), name.clone()).with_default_namespace())
        {
            return Err(ApiError::InvalidRequest {
                message: format!("duplicate Anthropic tool address: {}", tool.name),
            });
        }
        if !resolved.insert(tool.name.clone()) {
            return Err(ApiError::InvalidRequest {
                message: format!("Anthropic tool alias collision: {}", tool.name),
            });
        }
    }
    Ok(tools.into())
}

fn function(tool: &ResponsesApiTool, namespace: Option<&str>) -> Result<AnthropicTool, ApiError> {
    let input_schema =
        serde_json::to_value(&tool.parameters).map_err(|err| ApiError::InvalidRequest {
            message: format!("cannot encode Anthropic tool {} schema: {err}", tool.name),
        })?;
    let identity = AnthropicToolIdentity::Function {
        name: tool.name.clone(),
        namespace: namespace.map(str::to_owned),
    };
    Ok(AnthropicTool {
        name: identity.wire_alias()?,
        description: tool.description.clone(),
        input_schema: anthropic_function_input_schema(input_schema)?,
        identity,
    })
}

fn custom(tool: &FreeformTool, namespace: Option<&str>) -> Result<AnthropicTool, ApiError> {
    let identity = AnthropicToolIdentity::Custom {
        name: tool.name.clone(),
        namespace: namespace.map(str::to_owned),
    };
    Ok(AnthropicTool {
        name: identity.wire_alias()?,
        // A grammar constrains an OpenAI decoder; here the description alone states the format.
        description: format!("{}\n\nPass the raw input as the string `input`.", tool.description),
        input_schema: json!({
            "type": "object",
            "properties": {"input": {"type": "string"}},
            "required": ["input"],
            "additionalProperties": false,
        }),
        identity,
    })
}

#[cfg(test)]
#[path = "client_anthropic_tools_tests.rs"]
mod tests;
