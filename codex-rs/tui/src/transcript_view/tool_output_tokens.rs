//! Counts are measured once by the server from accepted, truncated model history.
//! Missing counts stay missing: a nested result or an old rollout is not an estimate of usage.

use super::TranscriptView;

impl TranscriptView {
    pub(crate) fn record_tool_output_tokens(
        &mut self,
        id: String,
        call_id: String,
        tokens: Option<u64>,
    ) {
        if self
            .tool_output_tokens
            .entry(call_id)
            .or_default()
            .insert(id, tokens)
            != Some(tokens)
        {
            self.cache.clear();
            self.live_key = None;
        }
    }

    pub(crate) fn activity_output_tokens(&self, ids: &[String]) -> Option<u64> {
        if !self.output_estimates_enabled || ids.is_empty() {
            return None;
        }
        ids.iter().try_fold(0u64, |total, id| {
            let (_, call_id) = id.split_once(':')?;
            self.tool_output_tokens
                .get(call_id)?
                .values()
                .try_fold(total, |total, tokens| {
                    tokens.map(|tokens| total.saturating_add(tokens))
                })
        })
    }
}
