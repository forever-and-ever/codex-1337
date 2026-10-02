use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Signed provider-native checkpoints are not OpenAI encrypted compaction items.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum NativeCompaction {
    Anthropic(AnthropicSignedCompaction),
}

/// An exact Anthropic summary/signature pair. Reject oversized blocks rather
/// than truncate either signed field. Only the content is visible to the model.
/// The owned summarization request caps output at 4096 provider tokens; this
/// decoded-byte bound is an additional resource limit, not a tokenizer claim.
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
    /// Output-token ceiling for the owned signed-summary request.
    pub const MAX_OUTPUT_TOKENS: std::num::NonZeroU32 =
        std::num::NonZeroU32::new(/*n*/ 4_096).unwrap();

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }
}
