use codex_protocol::DEFAULT_FUNCTION_NAMESPACE;
use codex_protocol::HookSelector;
use codex_protocol::ToolName;
use codex_protocol::protocol::HookEventName;

use super::HookMatcher;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolMatcher {
    namespace: Option<HookMatcher>,
    name: Option<HookMatcher>,
}

impl ToolMatcher {
    fn compile(selector: Option<&HookSelector>) -> Result<Option<Self>, String> {
        match selector {
            None => Ok(None),
            Some(HookSelector::Tool { namespace, name }) => Ok(Some(Self {
                namespace: namespace
                    .as_deref()
                    .map(HookMatcher::new)
                    .transpose()
                    .map_err(|err| err.to_string())?,
                name: name
                    .as_deref()
                    .map(HookMatcher::new)
                    .transpose()
                    .map_err(|err| err.to_string())?,
            })),
            Some(HookSelector::EventValue { .. }) => {
                Err("tool events require a tool selector".to_string())
            }
        }
    }

    fn matches(&self, tool: &ToolName) -> bool {
        let namespace = if tool.is_default_namespace() {
            DEFAULT_FUNCTION_NAMESPACE
        } else {
            tool.namespace.as_deref().expect("nondefault namespace")
        };
        self.namespace
            .as_ref()
            .is_none_or(|pattern| pattern.matches(Some(namespace)))
            && self
                .name
                .as_ref()
                .is_none_or(|pattern| pattern.matches(Some(&tool.name)))
    }
}

/// An admitted event and its only legal selector domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookSelection {
    PreToolUse(Option<ToolMatcher>),
    PermissionRequest(Option<ToolMatcher>),
    PostToolUse(Option<ToolMatcher>),
    PreCompact(Option<HookMatcher>),
    PostCompact(Option<HookMatcher>),
    SessionStart(Option<HookMatcher>),
    SessionEnd(Option<HookMatcher>),
    SubagentStart(Option<HookMatcher>),
    SubagentStop(Option<HookMatcher>),
    UserPromptSubmit,
    Stop,
    Interrupt,
}

impl HookSelection {
    pub(crate) fn compile(
        event: HookEventName,
        selector: Option<&HookSelector>,
    ) -> Result<Self, String> {
        let event_value = || match selector {
            None => Ok(None),
            Some(HookSelector::EventValue { pattern }) => HookMatcher::new(pattern)
                .map(Some)
                .map_err(|err| err.to_string()),
            Some(HookSelector::Tool { .. }) => {
                Err("lifecycle events require an event_value selector".to_string())
            }
        };
        let unfiltered = |selection| {
            if selector.is_some() {
                Err("this event does not accept a selector".to_string())
            } else {
                Ok(selection)
            }
        };
        match event {
            HookEventName::PreToolUse => ToolMatcher::compile(selector).map(Self::PreToolUse),
            HookEventName::PermissionRequest => {
                ToolMatcher::compile(selector).map(Self::PermissionRequest)
            }
            HookEventName::PostToolUse => ToolMatcher::compile(selector).map(Self::PostToolUse),
            HookEventName::PreCompact => event_value().map(Self::PreCompact),
            HookEventName::PostCompact => event_value().map(Self::PostCompact),
            HookEventName::SessionStart => event_value().map(Self::SessionStart),
            HookEventName::SessionEnd => event_value().map(Self::SessionEnd),
            HookEventName::SubagentStart => event_value().map(Self::SubagentStart),
            HookEventName::SubagentStop => event_value().map(Self::SubagentStop),
            HookEventName::UserPromptSubmit => unfiltered(Self::UserPromptSubmit),
            HookEventName::Stop => unfiltered(Self::Stop),
            HookEventName::Interrupt => unfiltered(Self::Interrupt),
        }
    }

    pub(crate) fn event_name(&self) -> HookEventName {
        match self {
            Self::PreToolUse(_) => HookEventName::PreToolUse,
            Self::PermissionRequest(_) => HookEventName::PermissionRequest,
            Self::PostToolUse(_) => HookEventName::PostToolUse,
            Self::PreCompact(_) => HookEventName::PreCompact,
            Self::PostCompact(_) => HookEventName::PostCompact,
            Self::SessionStart(_) => HookEventName::SessionStart,
            Self::SessionEnd(_) => HookEventName::SessionEnd,
            Self::SubagentStart(_) => HookEventName::SubagentStart,
            Self::SubagentStop(_) => HookEventName::SubagentStop,
            Self::UserPromptSubmit => HookEventName::UserPromptSubmit,
            Self::Stop => HookEventName::Stop,
            Self::Interrupt => HookEventName::Interrupt,
        }
    }

    pub(crate) fn matches_tool(&self, event: HookEventName, tool: Option<&ToolName>) -> bool {
        if self.event_name() != event {
            return false;
        }
        match self {
            Self::PreToolUse(matcher)
            | Self::PermissionRequest(matcher)
            | Self::PostToolUse(matcher) => matcher
                .as_ref()
                .is_none_or(|matcher| tool.is_some_and(|tool| matcher.matches(tool))),
            _ => false,
        }
    }

    pub(crate) fn matches_value(&self, event: HookEventName, value: Option<&str>) -> bool {
        if self.event_name() != event {
            return false;
        }
        match self {
            Self::PreCompact(matcher)
            | Self::PostCompact(matcher)
            | Self::SessionStart(matcher)
            | Self::SessionEnd(matcher)
            | Self::SubagentStart(matcher)
            | Self::SubagentStop(matcher) => matcher
                .as_ref()
                .is_none_or(|matcher| matcher.matches(value)),
            Self::UserPromptSubmit | Self::Stop | Self::Interrupt => true,
            Self::PreToolUse(_) | Self::PermissionRequest(_) | Self::PostToolUse(_) => false,
        }
    }
}

#[cfg(test)]
impl HookSelection {
    pub(crate) fn fixture(event: HookEventName, pattern: Option<HookMatcher>) -> Self {
        let selector = pattern.map(|pattern| match event {
            HookEventName::PreToolUse
            | HookEventName::PostToolUse
            | HookEventName::PermissionRequest => HookSelector::Tool {
                namespace: Some("^functions$".to_string()),
                name: Some(pattern.as_str().to_string()),
            },
            _ => HookSelector::EventValue {
                pattern: pattern.as_str().to_string(),
            },
        });
        Self::compile(event, selector.as_ref()).expect("valid fixture selector")
    }
}
