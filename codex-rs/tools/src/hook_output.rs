use crate::ToolName;
use codex_utils_path_uri::PathUri;
use serde_json::Value;

/// One completed tool invocation's hook-visible receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct PostToolUsePayload {
    pub tool_cwd: Option<PathUri>,
    pub tool_name: ToolName,
    pub tool_use_id: String,
    pub tool_input: Value,
    pub tool_response: Value,
}

impl PostToolUsePayload {
    pub fn new(
        tool_name: &ToolName,
        tool_use_id: &str,
        tool_input: Value,
        tool_response: Value,
    ) -> Self {
        Self {
            tool_cwd: None,
            tool_name: tool_name.clone(),
            tool_use_id: tool_use_id.to_string(),
            tool_input,
            tool_response,
        }
    }
}

/// Output-owned hook conversion. The handler still decides whether hooks participate.
/// A missing receipt permits the generic function handler's model-output fallback;
/// specialized handlers may instead wait for a completed invocation's receipt.
pub trait HookToolOutput {
    fn post_tool_use_payload(
        &self,
        _tool_name: &ToolName,
        _tool_use_id: &str,
        _tool_input: Value,
    ) -> Option<PostToolUsePayload> {
        None
    }
}

impl<T: HookToolOutput + ?Sized> HookToolOutput for Box<T> {
    fn post_tool_use_payload(
        &self,
        tool_name: &ToolName,
        tool_use_id: &str,
        tool_input: Value,
    ) -> Option<PostToolUsePayload> {
        (**self).post_tool_use_payload(tool_name, tool_use_id, tool_input)
    }
}

impl HookToolOutput for codex_protocol::mcp::CallToolResult {}

impl HookToolOutput for crate::JsonToolOutput {
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
            self.value.clone(),
        ))
    }
}
