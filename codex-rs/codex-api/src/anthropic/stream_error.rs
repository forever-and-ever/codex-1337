use crate::ResponseEvent;
use codex_client::TransportError;
use eventsource_stream::EventStreamError;
use thiserror::Error;
use tokio::time::error::Elapsed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicStreamProgress {
    BeforeOutput,
    PartialOutput,
    CompletedItem,
}

impl AnthropicStreamProgress {
    pub(super) fn after(self, event: &ResponseEvent) -> Self {
        match (self, event) {
            (Self::CompletedItem, _) | (_, ResponseEvent::OutputItemDone(_)) => Self::CompletedItem,
            (_, ResponseEvent::OutputItemAdded(_))
            | (_, ResponseEvent::OutputTextDelta(_))
            | (_, ResponseEvent::ToolCallInputDelta { .. })
            | (_, ResponseEvent::ReasoningSummaryDelta { .. })
            | (_, ResponseEvent::ReasoningSummaryDone { .. })
            | (_, ResponseEvent::ReasoningContentDelta { .. })
            | (_, ResponseEvent::ReasoningSummaryPartAdded { .. }) => Self::PartialOutput,
            (_, ResponseEvent::Created { .. })
            | (_, ResponseEvent::SafetyBuffering(_))
            | (_, ResponseEvent::ServerModel(_))
            | (_, ResponseEvent::ModelVerifications(_))
            | (_, ResponseEvent::TurnModerationMetadata(_))
            | (_, ResponseEvent::ServerReasoningIncluded(_))
            | (_, ResponseEvent::Completed { .. })
            | (_, ResponseEvent::RateLimits(_))
            | (_, ResponseEvent::ModelsEtag(_)) => self,
        }
    }
}

#[derive(Debug, Error)]
pub enum AnthropicStreamError {
    #[error("Anthropic SSE transport: {0}")]
    Transport(#[source] TransportError),
    #[error("Anthropic stream idle timeout: {0}")]
    IdleTimeout(#[source] Elapsed),
    #[error("Anthropic stream truncated before message_stop")]
    Truncated,
    #[error("invalid Anthropic SSE: {0}")]
    MalformedSse(#[source] EventStreamError<TransportError>),
}

impl AnthropicStreamError {
    pub(crate) fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport(
                TransportError::Network(_)
                    | TransportError::Connection(_)
                    | TransportError::Timeout
            ) | Self::IdleTimeout(_)
                | Self::Truncated
        )
    }
}
