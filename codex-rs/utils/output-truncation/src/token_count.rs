//! Local measurement only: this must not change the existing truncation policy.

use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;

/// Count completed, model-visible text using the bundled o200k vocabulary.
/// This is an approximation for newer OpenAI models, including Astra, not API usage.
/// Mixed media returns no count rather than presenting a partial total.
pub fn count_o200k_text_tokens(output: &FunctionCallOutputBody) -> Option<u64> {
    let tokenizer = tiktoken_rs::o200k_base_singleton();
    // Tool data spelling a special token is still ordinary untrusted text.
    match output {
        FunctionCallOutputBody::Text(text) => Some(tokenizer.count_ordinary(text) as u64),
        FunctionCallOutputBody::ContentItems(items) => {
            items.iter().try_fold(0u64, |total, item| match item {
                FunctionCallOutputContentItem::InputText { text } => {
                    Some(total.saturating_add(tokenizer.count_ordinary(text) as u64))
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
