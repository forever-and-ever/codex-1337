//! Bounded, human-only reasoning disclosure retained for one inference attempt.

use codex_protocol::ResponseThought;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;

pub(super) struct ThoughtAccumulator {
    pub(super) observed: Option<ResponseThought>,
    remaining: usize,
}

impl Default for ThoughtAccumulator {
    fn default() -> Self {
        Self {
            observed: None,
            remaining: 64 * 1024,
        }
    }
}

impl ThoughtAccumulator {
    pub(super) fn observe(&mut self, item: &ResponseItem) {
        let ResponseItem::Reasoning {
            summary, content, ..
        } = item
        else {
            return;
        };
        let thought = self.observed.get_or_insert_default();
        // Only readable provider text is eligible. encrypted_content is never inspected.
        let mut retain = |parts: &mut Vec<String>, text: &str| {
            let end = text.floor_char_boundary(self.remaining.min(text.len()));
            if end > 0 && !text[..end].trim().is_empty() {
                parts.push(text[..end].to_owned());
                self.remaining -= end;
            }
        };
        for ReasoningItemReasoningSummary::SummaryText { text } in summary {
            retain(&mut thought.summary, text);
        }
        for part in content.iter().flatten() {
            let (ReasoningItemContent::Text { text }
            | ReasoningItemContent::ReasoningText { text }) = part;
            retain(&mut thought.content, text);
        }
    }

    pub(super) fn finish(self, turn_id: &str, reported: Option<u64>) -> Option<ResponseThought> {
        let mut thought = match self.observed {
            Some(thought) => thought,
            None if reported.is_some_and(|count| count > 0) => ResponseThought::default(),
            None => return None,
        };
        thought.turn_id = turn_id.to_owned();
        thought.completed_at_ms = crate::turn_timing::now_unix_timestamp_ms();
        Some(thought)
    }
}
