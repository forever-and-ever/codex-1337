use super::ReasoningRemovalReason;
use super::SafetyBuffering;
use codex_protocol::ResponseItemId;
use codex_protocol::ResponseUsageMetadata;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ModelVerification;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TurnModerationMetadataEvent;
use std::collections::BTreeMap;

#[derive(Debug)]
pub enum ResponseEvent {
    Created {
        /// Existing server response ID, when supplied by the stream.
        response_id: Option<String>,
    },
    SafetyBuffering(SafetyBuffering),
    OutputItemDone(ResponseItem),
    OutputItemAdded(ResponseItem),
    /// Emitted when the server includes `OpenAI-Model` on the stream response.
    /// This can differ from the requested model when backend safety routing applies.
    ServerModel(String),
    /// Emitted when the server recommends additional account verification.
    ModelVerifications(Vec<ModelVerification>),
    /// Emitted when the server includes moderation metadata for first-party turn presentation.
    TurnModerationMetadata(TurnModerationMetadataEvent),
    /// Action-bound Guardian evidence for request-scoped consumers, not model history.
    GuardianAssessment(crate::guardian_assessment::GuardianAssessment),
    /// Emitted when `X-Reasoning-Included: true` is present on the response,
    /// meaning the server already accounted for past reasoning tokens and the
    /// client should not re-estimate them.
    ServerReasoningIncluded(bool),
    Completed {
        response_id: String,
        token_usage: Option<TokenUsage>,
        usage_metadata: Option<ResponseUsageMetadata>,
        /// Bounded diagnostic counts; absent when the provider reports no transformation data.
        reasoning_removals: Option<BTreeMap<ReasoningRemovalReason, i64>>,
        /// Did the model affirmatively end its turn? Some providers do not set this,
        /// so we rely on fallback logic when this is `None`.
        end_turn: Option<bool>,
    },
    OutputTextDelta {
        item_id: ResponseItemId,
        delta: String,
    },
    ToolCallInputDelta {
        item_id: ResponseItemId,
        call_id: Option<String>,
        delta: String,
    },
    ReasoningSummaryDelta {
        item_id: ResponseItemId,
        delta: String,
        summary_index: i64,
    },
    ReasoningSummaryDone {
        item_id: ResponseItemId,
        text: String,
        summary_index: i64,
    },
    ReasoningContentDelta {
        item_id: ResponseItemId,
        delta: String,
        content_index: i64,
    },
    ReasoningSummaryPartAdded {
        item_id: ResponseItemId,
        summary_index: i64,
    },
    RateLimits(RateLimitSnapshot),
    ModelsEtag(String),
}
