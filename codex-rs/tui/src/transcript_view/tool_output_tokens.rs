//! Counts are measured once by the server from accepted, truncated model history.
//! Missing counts stay missing: a nested result or an old rollout is not an estimate of usage.

use super::TranscriptView;
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Clone, Default, PartialEq, Eq)]
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
        self.tool_output_tokens.for_ids(ids)
    }
}

impl ToolOutputTokens {
    pub(super) fn subset(&self, ids: &[String]) -> Self {
        let mut subset = Self::default();
        for qualified in ids {
            let Some((kind, id)) = qualified.split_once(':') else {
                continue;
            };
            if kind == "output" {
                if let Some(count) = self.measurements.get(id) {
                    let _ = subset.measurements.insert(id.to_owned(), *count);
                }
            } else if let Some(outputs) = self.by_call.get(id) {
                let _ = subset.by_call.insert(id.to_owned(), outputs.clone());
                for output in outputs {
                    if let Some(count) = self.measurements.get(output) {
                        let _ = subset.measurements.insert(output.clone(), *count);
                    }
                }
            }
        }
        subset
    }

    pub(super) fn for_ids(&self, ids: &[String]) -> Option<u64> {
        if ids.is_empty() {
            return None;
        }
        let counts = self;
        let mut outputs = HashSet::new();
        // Returned batches own the bill. Nested execution IDs have no model-visible output of
        // their own; direct-call measurements, when present, still join the same deduplicated sum.
        let has_batches = ids.iter().any(|id| id.starts_with("output:"));
        for id in ids {
            let (kind, id) = id.split_once(':')?;
            if kind == "output" {
                let _ = outputs.insert(id);
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
