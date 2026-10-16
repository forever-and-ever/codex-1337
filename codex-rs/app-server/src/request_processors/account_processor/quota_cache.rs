//! Account-local polling shared by quota clients and ordinary statusline reads.

use codex_app_server_protocol::AccountProvider;
use codex_backend_client::RateLimitsWithResetCredits;
use codex_http_client::RetryAfter;
use codex_model_provider::AnthropicQuota;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;
use tokio::time::Instant;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct QuotaKey {
    pub provider: AccountProvider,
    pub account: String,
    // Reserve-capable reads can record backend exposure; passive polls cannot opt in.
    pub luna_reserve: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum QuotaUsage {
    Openai(RateLimitsWithResetCredits),
    Anthropic(AnthropicQuota),
}

pub(super) struct QuotaFailure {
    pub message: String,
    pub retry_after: Option<RetryAfter>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum QuotaFreshness {
    Cached,
    Refresh,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct QuotaRead {
    pub usage: Option<QuotaUsage>,
    pub captured_at: Option<i64>,
    pub error: Option<String>,
    pub retry_after_sec: u64,
}

#[derive(Default)]
struct Entry {
    read: QuotaRead,
    checked_at: Option<Instant>,
    next_attempt: Option<Instant>,
    failures: u32,
}

pub(super) struct QuotaCache {
    entries: Mutex<HashMap<QuotaKey, Arc<Mutex<Entry>>>>,
    permits: Semaphore,
}

impl Default for QuotaCache {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
            permits: Semaphore::new(/*permits*/ 4),
        }
    }
}

impl QuotaCache {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "the per-account lock single-flights its bounded refresh and cooldown commit"
    )]
    pub(super) async fn read<F, Fut>(
        self: &Arc<Self>,
        key: QuotaKey,
        freshness: QuotaFreshness,
        fetch: F,
    ) -> std::io::Result<QuotaRead>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<QuotaUsage, QuotaFailure>> + Send,
    {
        let requested_at = Instant::now();
        let entry = self.entries.lock().await.entry(key).or_default().clone();
        let cache = Arc::clone(self);
        // The cache owns a bounded refresh even when a short-lived CLI disconnects.
        // Cancellation cannot erase an upstream rejection and provoke an immediate retry.
        tokio::spawn(async move {
            let mut entry = entry.lock().await;
            let due = entry
                .next_attempt
                .is_none_or(|deadline| deadline <= Instant::now());
            let refresh = freshness == QuotaFreshness::Refresh
                && entry.read.error.is_none()
                && entry
                    .checked_at
                    .is_none_or(|checked| checked < requested_at);
            if due || refresh {
                let result = tokio::time::timeout(Duration::from_secs(/*secs*/ 4), async {
                    let _permit = cache.permits.acquire().await.map_err(|_| QuotaFailure {
                        message: "Quota reader stopped".into(),
                        retry_after: None,
                    })?;
                    fetch().await
                })
                .await
                .unwrap_or_else(|_| {
                    Err(QuotaFailure {
                        message: "Quota request timed out".into(),
                        retry_after: None,
                    })
                });
                let delay = match result {
                    Ok(usage) => {
                        entry.read = QuotaRead {
                            usage: Some(usage),
                            captured_at: Some(chrono::Utc::now().timestamp()),
                            ..Default::default()
                        };
                        entry.failures = 0;
                        Duration::from_secs(/*secs*/ 60)
                    }
                    Err(failure) => {
                        entry.failures = entry.failures.saturating_add(1);
                        entry.read.error = Some(failure.message);
                        let backoff = Duration::from_secs(60 * (1 << entry.failures.min(4)));
                        failure
                            .retry_after
                            .map_or(backoff, |retry| retry.remaining_delay().max(backoff))
                            .min(Duration::from_secs(/*secs*/ 86400))
                    }
                };
                let now = Instant::now();
                entry.checked_at = Some(now);
                entry.next_attempt = Some(now + delay);
            }
            let mut read = entry.read.clone();
            read.retry_after_sec = entry.next_attempt.map_or(0, |deadline| {
                let remaining = deadline.saturating_duration_since(Instant::now());
                remaining.as_secs() + u64::from(remaining.subsec_nanos() != 0)
            });
            read
        })
        .await
        .map_err(|_| std::io::Error::other("Quota refresh task failed"))
    }
}

#[cfg(test)]
#[path = "quota_cache_tests.rs"]
mod tests;
