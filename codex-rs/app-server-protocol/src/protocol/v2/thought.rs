//! One self-contained transcript projection for each reasoning-bearing response.

use super::ThreadItem;
use codex_protocol::protocol::RawResponseCompletedEvent;

impl ThreadItem {
    pub fn from_response_thought(event: &RawResponseCompletedEvent) -> Option<Self> {
        let thought = event.thought.as_ref()?;
        Some(Self::Thought {
            id: format!("thought:{}:{}", thought.turn_id, event.response_id),
            response_id: event.response_id.clone(),
            completed_at_ms: thought.completed_at_ms,
            summary: thought.summary.clone(),
            content: thought.content.clone(),
            reported_reasoning_tokens: event
                .usage_metadata
                .as_ref()
                .and_then(|metadata| metadata.reported_reasoning_tokens),
        })
    }
}
