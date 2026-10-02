use chrono::DateTime;
use chrono::Utc;
use codex_protocol::error::UsageLimitReachedError;
use http::HeaderMap;

/// A 429 alone cannot distinguish subscription exhaustion from temporary throttling.
/// Anthropic's overall admission decision includes any extra-usage allowance.
pub(crate) fn parse_usage_limit(headers: &HeaderMap) -> Option<UsageLimitReachedError> {
    let header = |name| headers.get(name).and_then(|value| value.to_str().ok());
    if header("anthropic-ratelimit-unified-status") != Some("rejected") {
        return None;
    }
    let (mut message, limit_window_minutes) =
        match header("anthropic-ratelimit-unified-representative-claim") {
            Some("seven_day") => (
                "You’ve hit your Claude weekly usage limit.".to_owned(),
                Some(10_080),
            ),
            Some("five_hour") => (
                "You’ve hit your Claude five-hour usage limit.".to_owned(),
                Some(300),
            ),
            _ => ("You’ve hit your Claude usage limit.".to_owned(), None),
        };
    if header("anthropic-ratelimit-unified-overage-status") == Some("rejected") {
        message.push_str(
            match header("anthropic-ratelimit-unified-overage-disabled-reason") {
                Some("out_of_credits") => " Extra usage credits are exhausted.",
                _ => " Extra usage is unavailable.",
            },
        );
    }
    let resets_at = header("anthropic-ratelimit-unified-reset")
        .and_then(|value| value.parse().ok())
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0));
    Some(UsageLimitReachedError {
        provider_message: Some(message),
        plan_type: None,
        resets_at,
        limit_window_minutes,
        rate_limits: None,
        promo_message: None,
        rate_limit_reached_type: None,
    })
}
