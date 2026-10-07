//! Human-only transcript items projected from one completed response.

use super::ThreadItem;
use codex_protocol::protocol::RawResponseCompletedEvent;

/// One transcript item a completed response contributes, with its turn and time.
pub struct ResponseProjection {
    pub turn_id: String,
    pub completed_at_ms: i64,
    pub item: ThreadItem,
}

impl ThreadItem {
    /// The census of the standing context the request carried, then the response's reasoning.
    pub fn from_response_completed(event: &RawResponseCompletedEvent) -> Vec<ResponseProjection> {
        let preamble = event.preamble.as_ref().map(|census| ResponseProjection {
            turn_id: census.turn_id.clone(),
            completed_at_ms: census.completed_at_ms,
            item: Self::Preamble {
                id: format!("preamble:{}:{}", census.turn_id, event.response_id),
                response_id: event.response_id.clone(),
                completed_at_ms: census.completed_at_ms,
                parts: census.parts.clone(),
                change: census.change.clone(),
                reported_input_tokens: event
                    .token_usage
                    .as_ref()
                    .and_then(|usage| u64::try_from(usage.input_tokens).ok()),
            },
        });
        let thought = event.thought.as_ref().map(|thought| ResponseProjection {
            turn_id: thought.turn_id.clone(),
            completed_at_ms: thought.completed_at_ms,
            item: Self::Thought {
                id: format!("thought:{}:{}", thought.turn_id, event.response_id),
                response_id: event.response_id.clone(),
                completed_at_ms: thought.completed_at_ms,
                summary: thought.summary.clone(),
                content: thought.content.clone(),
                reported_reasoning_tokens: event
                    .usage_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.reported_reasoning_tokens),
            },
        });
        preamble.into_iter().chain(thought).collect()
    }
}
