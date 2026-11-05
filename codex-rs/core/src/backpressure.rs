//! Waiting out provider backpressure: model capacity and rate limits.
//!
//! An overloaded or throttling provider asks its clients to come back later, so interactive
//! work waits with exponential backoff instead of failing, for as long as the user lets it.
//! Usage limits and exhausted quota are not backpressure: their windows last hours, so they
//! still end the turn.

use std::time::Duration;

use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use codex_client::RetryOperation;
use codex_http_client::RetryAfter;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::StreamErrorEvent;
use http::StatusCode;
use tokio::time::Instant;
use tracing::warn;

const FIRST_DELAY: Duration = Duration::from_secs(5);
const MAX_DELAY: Duration = Duration::from_mins(1);
/// Server advice can name an instant already past; never retry in a tight loop.
const MIN_DELAY: Duration = Duration::from_secs(1);

/// Why the provider pushes back.
enum Pushback<'a> {
    Capacity,
    PolicyCapacity,
    /// The provider's explanation, when it gave one.
    RateLimit(Option<&'a str>),
}

impl<'a> Pushback<'a> {
    fn of(error: &'a CodexErr) -> Option<Self> {
        match error.details() {
            CodexErrorDetails::ServerOverloaded => Some(Self::Capacity),
            CodexErrorDetails::PolicyCapacityUnavailable => Some(Self::PolicyCapacity),
            CodexErrorDetails::RateLimitExceeded(message) => Some(Self::RateLimit(
                Some(message.as_str()).filter(|message| !message.is_empty()),
            )),
            // A 429 that is neither a usage limit nor exhausted quota.
            CodexErrorDetails::RetryLimit(limit)
                if limit.status == StatusCode::TOO_MANY_REQUESTS =>
            {
                Some(Self::RateLimit(None))
            }
            _ => None,
        }
    }
}

/// Consecutive backpressure waits within one request loop.
#[derive(Default)]
pub(crate) struct Backpressure {
    waits: u32,
}

impl Backpressure {
    /// Waits out backpressure, or returns an error this policy does not own. Waits spend no
    /// retry budget. Internal and Guardian sessions keep their failure path: adjudication must
    /// not wait indefinitely.
    pub(crate) async fn wait(
        &mut self,
        err: CodexErr,
        sess: &Session,
        turn_context: &TurnContext,
        operation: RetryOperation,
    ) -> Result<(), CodexErr> {
        let Some(pushback) = Pushback::of(&err) else {
            return Err(err);
        };
        if turn_context.session_source.is_internal()
            || crate::guardian::is_basic_session_source(&turn_context.session_source)
        {
            return Err(err);
        }
        self.waits = self.waits.saturating_add(1);
        // Server advice wins; otherwise the wait doubles from five seconds to one minute.
        let now = Instant::now();
        let retry_at = err
            .retry_after()
            .map_or_else(
                || {
                    now + FIRST_DELAY
                        .saturating_mul(2u32.saturating_pow(self.waits - 1))
                        .min(MAX_DELAY)
                },
                RetryAfter::deadline,
            )
            .max(now + MIN_DELAY);
        let delay = retry_at.saturating_duration_since(now);
        let (headline, codex_error_info, additional_details) = match pushback {
            // The error's own text advises another model, which this wait makes moot.
            Pushback::Capacity => (
                "Selected model is at capacity.",
                CodexErrorInfo::ServerOverloaded,
                None,
            ),
            Pushback::PolicyCapacity => (
                "Policy-restricted capacity is unavailable.",
                CodexErrorInfo::PolicyCapacityUnavailable,
                None,
            ),
            Pushback::RateLimit(explanation) => (
                "Rate limited by the model provider.",
                CodexErrorInfo::RateLimitExceeded,
                explanation.map(str::to_owned),
            ),
        };
        warn!(turn_id = %turn_context.sub_id, error = %err, ?delay, waits = self.waits, "provider backpressure; waiting to retry");
        sess.send_event(
            turn_context,
            EventMsg::StreamError(StreamErrorEvent {
                message: format!(
                    "{headline} Retrying in {}s (retry {})...",
                    delay.as_secs() + u64::from(delay.subsec_nanos() > 0),
                    self.waits
                ),
                codex_error_info: Some(codex_error_info),
                additional_details,
            }),
        )
        .await;
        codex_client::record_retry!(self.waits, delay, operation);
        tokio::time::sleep_until(retry_at).await;
        Ok(())
    }
}
