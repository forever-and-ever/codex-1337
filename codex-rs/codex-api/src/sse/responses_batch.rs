//! Projects a finalized nonstreaming Responses object. No item is admitted from
//! an unsuccessful or malformed row; a successful row uses the normal item identities.
use super::ResponsesEventError;
use super::identity::ResponsesItemIdentity;
use super::process_responses_event;
use crate::ApiError;
use crate::ResponseEvent;
use serde_json::Value;
use serde_json::json;

pub fn batch_response_events(mut body: Value) -> Result<Vec<ResponseEvent>, ApiError> {
    let invalid = || ApiError::Stream("invalid OpenRouter batch response".into());
    let mut identity = ResponsesItemIdentity::default();
    let mut parse = |event| {
        process_responses_event(
            serde_json::from_value(event).map_err(|_| invalid())?,
            &mut identity,
        )
        .map_err(ResponsesEventError::into_api_error)?
        .ok_or_else(invalid)
    };
    let status = match body.get("status").and_then(Value::as_str) {
        Some("completed") => "response.completed",
        Some("failed") => "response.failed",
        Some("incomplete") => "response.incomplete",
        _ => return Err(invalid()),
    };
    let id = body
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(invalid)?
        .to_owned();
    // Failed/incomplete rows cannot authorize tool execution, even when they contain calls.
    if status != "response.completed" {
        let terminal = parse(json!({"type":status,"response":body}))?;
        return Ok(vec![terminal]);
    }
    let output = body
        .get_mut("output")
        .map(Value::take)
        .ok_or_else(invalid)?;
    let Value::Array(output) = output else {
        return Err(invalid());
    };
    let mut events = vec![ResponseEvent::Created {
        response_id: Some(id),
    }];
    for item in output {
        if item
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|s| s != "completed")
        {
            return Err(invalid());
        }
        events.push(parse(
            json!({"type":"response.output_item.added","item":item}),
        )?);
        events.push(parse(
            json!({"type":"response.output_item.done","item":item}),
        )?);
    }
    events.push(parse(json!({"type":status,"response":body}))?);
    Ok(events)
}
