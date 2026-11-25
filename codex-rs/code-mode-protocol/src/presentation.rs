//! Provider references are documentation; JavaScript members remain runtime identities.
use codex_protocol::ToolName;

use crate::ToolDefinition;
use crate::normalize_code_mode_identifier;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolDialect {
    #[default]
    Responses,
    Anthropic,
    /// Unknown providers get valid JS names, not guessed native syntax.
    Other,
}

impl ToolDialect {
    pub(crate) fn reference(self, name: &ToolName, member: &str) -> String {
        match self {
            Self::Responses => name.to_string(),
            Self::Anthropic | Self::Other => member.to_owned(),
        }
    }

    pub(crate) fn heading(self, tool: &ToolDefinition) -> String {
        let member = normalize_code_mode_identifier(&tool.name);
        let reference = self.reference(&tool.tool_name, &member);
        if reference == member {
            format!("tools.{member}")
        } else {
            format!("{reference} (tools.{member})")
        }
    }

    pub(crate) fn contract_heading(self, tool: &ToolDefinition) -> String {
        let member = normalize_code_mode_identifier(&tool.name);
        let reference = self.reference(&tool.tool_name, &member);
        if reference == member {
            format!("### `{member}`")
        } else {
            format!("### `{member}` (`{reference}`)")
        }
    }
}
