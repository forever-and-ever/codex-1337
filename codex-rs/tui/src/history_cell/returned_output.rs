//! Human-facing projection of Code Mode's execution envelopes, never of command stdout.
//! Keep the original body for raw history and token accounting.

use serde_json::Value;
use std::borrow::Cow;

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
