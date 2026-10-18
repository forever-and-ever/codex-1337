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

#[derive(Clone, Copy)]
pub enum ProviderPredicate {
    For(ToolDialect),
    Except(ToolDialect),
}

impl ProviderPredicate {
    pub fn matches(self, dialect: ToolDialect) -> bool {
        match self {
            Self::For(provider) => dialect == provider,
            Self::Except(provider) => dialect != provider,
        }
    }
}

pub struct ProviderNote {
    pub predicate: ProviderPredicate,
    pub text: &'static str,
}

impl ToolDialect {
    pub(crate) fn notes(self) -> impl Iterator<Item = &'static str> {
        const NOTES: &[ProviderNote] = &[
            ProviderNote {
                predicate: ProviderPredicate::For(ToolDialect::Anthropic),
                text: "Native provider tool names may be flat aliases; the inventory below gives JavaScript members, not native aliases.",
            },
            ProviderNote {
                predicate: ProviderPredicate::Except(ToolDialect::Anthropic),
                text: "Namespace references are not JavaScript paths. Invoke the exact tools member shown in the inventory.",
            },
        ];
        NOTES
            .iter()
            .filter(move |note| note.predicate.matches(self))
            .map(|note| note.text)
    }

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
