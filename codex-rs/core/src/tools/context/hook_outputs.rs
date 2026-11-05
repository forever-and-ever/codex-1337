use super::AbortedToolOutput;
use super::ApplyPatchToolOutput;
use super::ExecCommandToolOutput;
use super::FunctionToolOutput;
use super::McpToolOutput;
use super::ToolSearchOutput;
use codex_tools::HookToolOutput;
use codex_tools::PostToolUsePayload;
use codex_tools::ToolName;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ExecHookContext {
    pub tool_name: ToolName,
    pub command: String,
}

impl HookToolOutput for ToolSearchOutput {}
impl HookToolOutput for AbortedToolOutput {}

impl HookToolOutput for McpToolOutput {
    fn post_tool_use_payload(
        &self,
        tool_name: &ToolName,
        tool_use_id: &str,
        _tool_input: Value,
    ) -> Option<PostToolUsePayload> {
        Some(PostToolUsePayload::new(
            tool_name,
            tool_use_id,
            self.tool_input.clone(),
            serde_json::to_value(&self.result).ok()?,
        ))
    }
}

impl HookToolOutput for FunctionToolOutput {
    fn post_tool_use_payload(
        &self,
        tool_name: &ToolName,
        tool_use_id: &str,
        tool_input: Value,
    ) -> Option<PostToolUsePayload> {
        Some(PostToolUsePayload::new(
            tool_name,
            tool_use_id,
            tool_input,
            self.post_tool_use_response.clone()?,
        ))
    }
}

impl HookToolOutput for ApplyPatchToolOutput {
    fn post_tool_use_payload(
        &self,
        tool_name: &ToolName,
        tool_use_id: &str,
        tool_input: Value,
    ) -> Option<PostToolUsePayload> {
        Some(PostToolUsePayload::new(
            tool_name,
            tool_use_id,
            tool_input,
            Value::String(self.text.clone()),
        ))
    }
}

impl HookToolOutput for ExecCommandToolOutput {
    fn post_tool_use_payload(
        &self,
        _tool_name: &ToolName,
        _tool_use_id: &str,
        _tool_input: Value,
    ) -> Option<PostToolUsePayload> {
        if self.process_id.is_some() {
            return None;
        }
        let context = self.hook_context.as_ref()?;
        let mut payload = PostToolUsePayload::new(
            &context.tool_name,
            &self.event_call_id,
            serde_json::json!({ "command": context.command }),
            Value::String(self.truncated_output_with_policy(self.model_output_policy())),
        );
        payload.tool_cwd.clone_from(&self.tool_cwd);
        Some(payload)
    }
}
