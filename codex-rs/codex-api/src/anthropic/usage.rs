use crate::ApiError;
use codex_protocol::protocol::TokenUsage;
use serde::Deserialize;
use serde_json::Value;

#[derive(Default, Deserialize)]
pub(super) struct Usage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    iterations: Option<Vec<Usage>>,
}

impl Usage {
    pub(super) fn update(&mut self, raw: &Value) -> Result<(), ApiError> {
        let update: Self = serde_json::from_value(raw.clone())
            .map_err(|e| ApiError::NativeStream(format!("invalid Anthropic usage: {e}")))?;
        for count in [
            update.input_tokens,
            update.output_tokens,
            update.cache_creation_input_tokens,
            update.cache_read_input_tokens,
        ]
        .into_iter()
        .flatten()
        {
            if count < 0 {
                return Err(ApiError::NativeStream(
                    "negative Anthropic token count".into(),
                ));
            }
        }
        // Message deltas report cumulative counts, not increments.
        self.input_tokens = update.input_tokens.or(self.input_tokens);
        self.output_tokens = update.output_tokens.or(self.output_tokens);
        self.cache_creation_input_tokens = update
            .cache_creation_input_tokens
            .or(self.cache_creation_input_tokens);
        self.cache_read_input_tokens = update
            .cache_read_input_tokens
            .or(self.cache_read_input_tokens);
        self.iterations = update.iterations.or(self.iterations.take());
        Ok(())
    }

    pub(super) fn finish(&self) -> Result<TokenUsage, ApiError> {
        // Iterations are the accounting authority when present. Adding the
        // top-level summary would count the same inference twice.
        let mut counts = [0i64; 4];
        let parts = self
            .iterations
            .as_deref()
            .unwrap_or(std::slice::from_ref(self));
        if parts.is_empty() {
            return Err(ApiError::NativeStream(
                "Anthropic usage iterations missing".into(),
            ));
        }
        for part in parts {
            let input = part
                .input_tokens
                .ok_or_else(|| ApiError::NativeStream("Anthropic input usage missing".into()))?;
            let output = part
                .output_tokens
                .ok_or_else(|| ApiError::NativeStream("Anthropic output usage missing".into()))?;
            for (total, value) in counts.iter_mut().zip([
                input,
                part.cache_creation_input_tokens.unwrap_or(0),
                part.cache_read_input_tokens.unwrap_or(0),
                output,
            ]) {
                if value < 0 {
                    return Err(ApiError::NativeStream(
                        "negative Anthropic token count".into(),
                    ));
                }
                *total = total.checked_add(value).ok_or_else(|| {
                    ApiError::NativeStream("Anthropic token count overflow".into())
                })?;
            }
        }
        let [fresh, write, read, output] = counts;
        let input = fresh.checked_add(write).and_then(|n| n.checked_add(read));
        let input =
            input.ok_or_else(|| ApiError::NativeStream("Anthropic input overflow".into()))?;
        let total = input
            .checked_add(output)
            .ok_or_else(|| ApiError::NativeStream("Anthropic total overflow".into()))?;
        Ok(TokenUsage {
            input_tokens: input,
            cached_input_tokens: read,
            cache_write_input_tokens: write,
            output_tokens: output,
            reasoning_output_tokens: 0,
            total_tokens: total,
            codex_rollout_budget_units: None,
        })
    }
}
