//! The human-only transcript item projected from one completed response.

use super::ThreadItem;
use super::ThreadItemPayload;
use codex_protocol::protocol::RawResponseCompletedEvent;

/// The Thought a completed response contributes, with its turn and time.
///
/// A rollout line projects to at most one item: the thread-history store pages items by the
/// ordinal of the line that created them.
pub struct ResponseProjection {
    pub turn_id: String,
    pub completed_at_ms: i64,
    pub item: ThreadItem,
}

impl ThreadItem {
    /// The response's reasoning, when it carried any.
    pub fn from_response_completed(
        event: &RawResponseCompletedEvent,
    ) -> Option<ResponseProjection> {
        let thought = event.thought.as_ref()?;
        Some(ResponseProjection {
            turn_id: thought.turn_id.clone(),
            completed_at_ms: thought.completed_at_ms,
            item: Self {
                started_at_ms: None,
                completed_at_ms: (thought.completed_at_ms != 0).then_some(thought.completed_at_ms),
                payload: ThreadItemPayload::Thought {
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
            },
        })
    }
}
