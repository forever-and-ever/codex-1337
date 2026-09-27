//! Counts are measured once by the server from accepted, truncated model history.
//! Missing counts stay missing: a nested result or an old rollout is not an estimate of usage.

use super::TranscriptView;
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct ToolOutputTokens {
    by_call: HashMap<String, HashSet<String>>,
    measurements: HashMap<String, Option<u64>>,
}

impl TranscriptView {
    pub(crate) fn record_tool_output_tokens(
        &mut self,
        id: String,
        call_id: String,
        tokens: Option<u64>,
    ) {
        let counts = &mut self.tool_output_tokens;
        let new_output = counts
            .by_call
            .entry(call_id)
            .or_default()
            .insert(id.clone());
        if counts.measurements.insert(id, tokens) != Some(tokens) || new_output {
            self.cache.clear();
            self.live_key = None;
        }
    }

    pub(crate) fn activity_output_tokens(&self, ids: &[String]) -> Option<u64> {
        if !self.output_estimates_enabled || ids.is_empty() {
            return None;
        }
        let counts = &self.tool_output_tokens;
        let mut outputs = HashSet::new();
        // Returned batches own the bill. Nested execution IDs have no model-visible output of
        // their own; direct-call measurements, when present, still join the same deduplicated sum.
        let has_batches = ids.iter().any(|id| id.starts_with("output:"));
        for id in ids {
            let (kind, id) = id.split_once(':')?;
            if kind == "output" {
                outputs.insert(id);
            } else {
                match counts.by_call.get(id) {
                    Some(ids) => outputs.extend(ids.iter().map(String::as_str)),
                    None if has_batches => {}
                    None => return None,
                }
            }
        }
        outputs.into_iter().try_fold(0u64, |total, id| {
            counts
                .measurements
                .get(id)?
                .map(|tokens| total.saturating_add(tokens))
        })
    }
}
