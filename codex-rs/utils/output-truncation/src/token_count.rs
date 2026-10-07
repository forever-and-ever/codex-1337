//! Local measurement only: this must not change the existing truncation policy.

use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;

/// Count model-visible text using the bundled o200k vocabulary. This is an
/// approximation for OpenAI and Claude models, not API usage.
pub fn count_o200k_text(text: &str) -> u64 {
    // Text spelling a special token is still ordinary untrusted text.
    tiktoken_rs::o200k_base_singleton().count_ordinary(text) as u64
}

/// Count completed tool output with `count_o200k_text`.
/// Mixed media returns no count rather than presenting a partial total.
pub fn count_o200k_text_tokens(output: &FunctionCallOutputBody) -> Option<u64> {
    match output {
        FunctionCallOutputBody::Text(text) => Some(count_o200k_text(text)),
        FunctionCallOutputBody::ContentItems(items) => {
            items.iter().try_fold(0u64, |total, item| match item {
                FunctionCallOutputContentItem::InputText { text } => {
                    Some(total.saturating_add(count_o200k_text(text)))
                }
                FunctionCallOutputContentItem::InputImage { .. }
                | FunctionCallOutputContentItem::InputAudio { .. }
                | FunctionCallOutputContentItem::EncryptedContent { .. } => None,
            })
        }
    }
}

#[cfg(test)]
#[path = "token_count_tests.rs"]
mod tests;
