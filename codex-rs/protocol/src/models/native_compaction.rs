#![expect(
    unused_qualifications,
    reason = "A derive-generated variant field reference has no qualified source segments and rustc suggests an empty removal span"
)]

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Signed provider-native checkpoints are not `OpenAI` encrypted compaction items.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum NativeCompaction {
    Anthropic(AnthropicSignedCompaction),
}

/// An exact Anthropic summary/signature pair. Reject oversized blocks rather
/// than truncate either signed field. Only the content is visible to the model.
/// The request's generation budget includes internal thinking. The decoded-byte
/// bound limits the retained summary independently; it is not a tokenizer claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(try_from = "AnthropicSignedCompactionWire")]
pub struct AnthropicSignedCompaction {
    content: String,
    signature: String,
}

#[derive(Deserialize)]
struct AnthropicSignedCompactionWire {
    content: String,
    signature: String,
}

impl TryFrom<AnthropicSignedCompactionWire> for AnthropicSignedCompaction {
    type Error = &'static str;

    fn try_from(value: AnthropicSignedCompactionWire) -> Result<Self, Self::Error> {
        if value.content.is_empty() || value.content.len() > 32_768 {
            return Err("native compaction content must contain 1..=32768 UTF-8 bytes");
        }
        if value.signature.is_empty() || value.signature.len() > 65_536 {
            return Err("native compaction signature must contain 1..=65536 bytes");
        }
        Ok(Self {
            content: value.content,
            signature: value.signature,
        })
    }
}

impl AnthropicSignedCompaction {
    /// Generation-token ceiling, including thinking before the signed summary.
    pub const MAX_OUTPUT_TOKENS: std::num::NonZeroU32 =
        std::num::NonZeroU32::new(/*n*/ 32_768).unwrap();

    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    #[must_use]
    pub fn signature(&self) -> &str {
        &self.signature
    }
}
