use codex_protocol::HookSelector;
use serde_json::Value;

/// Converts only foreign filters whose existing imported meaning is preserved.
/// Tool filters cannot be projected onto a structured callable address.
pub(super) fn import_selector(
    event_name: &str,
    matcher: Option<&Value>,
) -> Result<Option<HookSelector>, &'static str> {
    let Some(matcher) = matcher else {
        return Ok(None);
    };
    let Some(pattern) = matcher.as_str() else {
        return Err("matcher must be a string");
    };
    // These foreign filters already import as match-all, including unattributed requests.
    if pattern.is_empty() || pattern == "*" {
        return Ok(None);
    }
    match event_name {
        "SessionStart" | "SessionEnd" | "PreCompact" | "PostCompact" | "SubagentStart"
        | "SubagentStop" => Ok(Some(HookSelector::EventValue {
            pattern: pattern.to_string(),
        })),
        "PreToolUse" | "PermissionRequest" | "PostToolUse" => {
            Err("foreign tool filter cannot identify a native namespace and name independently")
        }
        _ => Err("this event does not support a selector"),
    }
}
