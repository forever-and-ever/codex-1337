//! Human-facing projection of Code Mode's execution envelopes, never of command stdout.
//! Keep the original body for raw history and token accounting.

use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use serde_json::Value;
use std::borrow::Cow;

#[derive(Debug)]
pub(super) enum WaitOutcome {
    Running,
    Completed,
    Exited(i64),
    Failed,
    Terminated,
    Unknown,
}

/// Empty Code Mode results still carry useful wait status in their transport envelope.
/// Inspect only envelopes, never stdout, and don't summarize several process results as one.
pub(super) fn empty_wait_outcome(
    name: &str,
    output: &FunctionCallOutputBody,
) -> Option<WaitOutcome> {
    let texts: Vec<&str> = match output {
        FunctionCallOutputBody::Text(text) => vec![text],
        FunctionCallOutputBody::ContentItems(items) => items
            .iter()
            .filter_map(|item| match item {
                FunctionCallOutputContentItem::InputText { text } => Some(text.as_str()),
                _ => None,
            })
            .collect(),
    };
    let mut outcome = (name == "wait").then_some(WaitOutcome::Unknown);
    let mut processes = Vec::new();
    for text in texts {
        let body = if let Some((header, body)) = text.split_once("\nOutput:\n")
            && header.contains("\nWall time")
        {
            if header.starts_with("Script running with cell ID ") {
                outcome = Some(WaitOutcome::Running);
            } else if name == "wait" {
                outcome = Some(match header.lines().next() {
                    Some("Script completed") => WaitOutcome::Completed,
                    Some("Script failed") => WaitOutcome::Failed,
                    Some("Script terminated") => WaitOutcome::Terminated,
                    _ => WaitOutcome::Unknown,
                });
            }
            body
        } else {
            text
        };
        for value in serde_json::Deserializer::from_str(body).into_iter::<Value>() {
            let Ok(value) = value else { break };
            if value.get("output").and_then(Value::as_str) != Some("")
                || value
                    .get("wall_time_seconds")
                    .and_then(Value::as_f64)
                    .is_none()
            {
                continue;
            }
            if let Some(code) = value.get("exit_code").and_then(Value::as_i64) {
                processes.push(WaitOutcome::Exited(code));
            } else if value.get("session_id").and_then(Value::as_u64).is_some() {
                processes.push(WaitOutcome::Running);
            }
        }
    }
    match processes.len() {
        0 => outcome,
        1 => processes.pop(),
        _ => Some(WaitOutcome::Unknown),
    }
}

pub(super) fn readable_output(text: &str) -> Cow<'_, str> {
    let text = if (text.starts_with("Script completed\n")
        || text.starts_with("Script running with cell ID ")
        || text.starts_with("Script failed\n")
        || text.starts_with("Script terminated\n"))
        && let Some((header, body)) = text.split_once("\nOutput:\n")
        && header.contains("\nWall time")
    {
        body
    } else {
        text
    };
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let mut bodies = Vec::new();
    while let Some(Ok(value)) = stream.next() {
        let Some(body) = result_body(&value) else {
            return Cow::Borrowed(text);
        };
        if !body.trim().is_empty() {
            bodies.push(body);
        }
        if text[stream.byte_offset()..].trim().is_empty() {
            return Cow::Owned(bodies.join("\n"));
        }
    }
    Cow::Borrowed(text)
}

fn result_body(value: &Value) -> Option<String> {
    let fields = value.as_object()?;
    if fields.is_empty() {
        return Some(String::new());
    }
    if let Some(body) = fields.get("output").and_then(Value::as_str)
        && (fields.contains_key("exit_code") || fields.contains_key("wall_time_seconds"))
    {
        // This is stdout/stderr, not another transport envelope. Preserve JSON files verbatim.
        return Some(body.to_owned());
    }
    let blocks = fields.get("content")?.as_array()?;
    let mut bodies = Vec::new();
    for block in blocks {
        let body = match block.get("type")?.as_str()? {
            "text" => block.get("text")?.as_str()?.to_owned(),
            "image" => "<image>".to_owned(),
            "audio" => "<audio>".to_owned(),
            "resource_link" => format!("link: {}", block.get("uri")?.as_str()?),
            "resource" => {
                let resource = block.get("resource")?;
                resource
                    .get("text")
                    .or_else(|| resource.get("uri"))?
                    .as_str()?
                    .to_owned()
            }
            _ => continue,
        };
        if !body.trim().is_empty() {
            bodies.push(body);
        }
    }
    Some(bodies.join("\n"))
}
