//! Recognize only the separate, outer Code Mode execution header. Emitted text is opaque.

use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use serde_json::Value;

#[derive(Debug)]
pub(super) enum WaitOutcome {
    Running,
    Completed,
    Failed,
    Terminated,
    Unknown,
}

/// Empty Code Mode results still carry useful wait status in the constructor's header.
pub(super) fn empty_wait_outcome(
    name: &str,
    output: &FunctionCallOutputBody,
) -> Option<WaitOutcome> {
    let header = match output {
        FunctionCallOutputBody::Text(text) => Some(text.as_str()),
        FunctionCallOutputBody::ContentItems(items) => match items.first() {
            Some(FunctionCallOutputContentItem::InputText { text }) => Some(text.as_str()),
            _ => None,
        },
    };
    let Some(header) = header.filter(|text| is_execution_header(text)) else {
        return (name == "wait").then_some(WaitOutcome::Unknown);
    };
    if header.starts_with("Script running with cell ID ") {
        return Some(WaitOutcome::Running);
    }
    (name == "wait").then(|| match header.lines().next() {
        Some("Script completed") => WaitOutcome::Completed,
        Some("Script failed") => WaitOutcome::Failed,
        Some("Script terminated") => WaitOutcome::Terminated,
        _ => WaitOutcome::Unknown,
    })
}

pub(super) fn is_execution_header(text: &str) -> bool {
    (text.starts_with("Script completed\n")
        || text.starts_with("Script running with cell ID ")
        || text.starts_with("Script failed\n")
        || text.starts_with("Script terminated\n"))
        && text
            .split_once("\nOutput:\n")
            .is_some_and(|(header, body)| {
                let mut lines = header.lines();
                lines.next();
                body.is_empty()
                    && match lines.next() {
                        None => true,
                        Some(timing) => timing.starts_with("Wall time ") && lines.next().is_none(),
                    }
            })
}

/// Render the domain payload, not the MCP envelope. Some servers also serialize the
/// same value into a text block for older clients; show that result only once.
pub(super) fn structured_text<'a>(
    value: &Value,
    text: impl Iterator<Item = &'a str>,
) -> Option<String> {
    if value.is_null()
        || text.into_iter().any(|text| {
            value.as_str() == Some(text)
                || serde_json::from_str::<Value>(text).is_ok_and(|parsed| parsed == *value)
        })
    {
        return None;
    }
    match value {
        Value::String(text) => Some(text.clone()),
        _ => serde_json::to_string_pretty(value).ok(),
    }
}
