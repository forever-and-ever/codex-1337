use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolPayload;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;

pub(super) fn mismatch_message(
    spec: &ToolSpec,
    tool_name: &ToolName,
    payload: &ToolPayload,
) -> Result<String, FunctionCallError> {
    let expected = match spec {
        ToolSpec::Freeform(_) => "freeform text input",
        ToolSpec::Function(_) => "JSON arguments",
        ToolSpec::ToolSearch { .. } => "tool-search arguments",
        ToolSpec::Namespace(namespace) => namespace
            .tools
            .iter()
            .find_map(|tool| match tool {
                ResponsesApiNamespaceTool::Function(tool) => {
                    (tool.name == tool_name.name).then_some("JSON arguments")
                }
                ResponsesApiNamespaceTool::Custom(tool) => {
                    (tool.name == tool_name.name).then_some("freeform text input")
                }
            })
            .ok_or_else(|| {
                FunctionCallError::Fatal(format!(
                    "tool {tool_name} is missing from its namespace specification"
                ))
            })?,
        ToolSpec::WebSearch { .. } => {
            return Err(FunctionCallError::Fatal(format!(
                "tool {tool_name} has no locally callable input specification"
            )));
        }
    };
    let actual = match payload {
        ToolPayload::Function { .. } => "JSON arguments",
        ToolPayload::ToolSearch { .. } => "tool-search arguments",
        ToolPayload::Custom { .. } => "freeform text input",
    };
    Ok(format!("{tool_name} expects {expected}, not {actual}"))
}
