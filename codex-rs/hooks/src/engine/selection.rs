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
    fn compile(selector: &HookSelector) -> Result<Self, String> {
        match selector {
            HookSelector::Tool { namespace, name } => Ok(Self {
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
            }),
            HookSelector::EventValue { .. } => {
                Err("tool events require a tool selector".to_string())
            }
        }
    }

    fn matches(&self, tool: &ToolName) -> bool {
        let namespace = match tool.namespace.as_deref() {
            Some(namespace) if !tool.is_default_namespace() => namespace,
            _ => DEFAULT_FUNCTION_NAMESPACE,
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
    /// Selects every occurrence of `event`.
    pub(crate) fn unfiltered(event: HookEventName) -> Self {
        match event {
            HookEventName::PreToolUse => Self::PreToolUse(None),
            HookEventName::PermissionRequest => Self::PermissionRequest(None),
            HookEventName::PostToolUse => Self::PostToolUse(None),
            HookEventName::PreCompact => Self::PreCompact(None),
            HookEventName::PostCompact => Self::PostCompact(None),
            HookEventName::SessionStart => Self::SessionStart(None),
            HookEventName::SessionEnd => Self::SessionEnd(None),
            HookEventName::SubagentStart => Self::SubagentStart(None),
            HookEventName::SubagentStop => Self::SubagentStop(None),
            HookEventName::UserPromptSubmit => Self::UserPromptSubmit,
            HookEventName::Stop => Self::Stop,
            HookEventName::Interrupt => Self::Interrupt,
        }
    }

    pub(crate) fn compile(
        event: HookEventName,
        selector: Option<&HookSelector>,
    ) -> Result<Self, String> {
        let Some(selector) = selector else {
            return Ok(Self::unfiltered(event));
        };
        let tool = || ToolMatcher::compile(selector).map(Some);
        let event_value = || match selector {
            HookSelector::EventValue { pattern } => HookMatcher::new(pattern)
                .map(Some)
                .map_err(|err| err.to_string()),
            HookSelector::Tool { .. } => {
                Err("lifecycle events require an event_value selector".to_string())
            }
        };
        match event {
            HookEventName::PreToolUse => tool().map(Self::PreToolUse),
            HookEventName::PermissionRequest => tool().map(Self::PermissionRequest),
            HookEventName::PostToolUse => tool().map(Self::PostToolUse),
            HookEventName::PreCompact => event_value().map(Self::PreCompact),
            HookEventName::PostCompact => event_value().map(Self::PostCompact),
            HookEventName::SessionStart => event_value().map(Self::SessionStart),
            HookEventName::SessionEnd => event_value().map(Self::SessionEnd),
            HookEventName::SubagentStart => event_value().map(Self::SubagentStart),
            HookEventName::SubagentStop => event_value().map(Self::SubagentStop),
            HookEventName::UserPromptSubmit | HookEventName::Stop | HookEventName::Interrupt => {
                Err("this event does not accept a selector".to_string())
            }
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
