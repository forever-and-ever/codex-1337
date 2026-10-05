use super::*;
use codex_protocol::protocol::RateLimitWindow;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio::sync::Notify;

fn usage(used_percent: f64) -> QuotaUsage {
    QuotaUsage::Anthropic(AnthropicQuota {
        five_hour: Some(RateLimitWindow {
            used_percent,
            window_minutes: Some(300),
            resets_at: Some(2000000000),
        }),
        seven_day: None,
    })
}

#[tokio::test(start_paused = true)]
async fn cancelled_clients_cannot_erase_refreshes_or_failure_cooldowns() {
    // Status-bar subprocesses have short deadlines. Cancelling one must neither
    // lose a refresh nor let the next poll bypass the account's backoff.
    let cache = Arc::new(QuotaCache::default());
    let key = QuotaKey {
        provider: SubscriptionAccountProvider::ClaudeSubscription,
        account: "alice".into(),
        luna_reserve: false,
    };
    let started = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let attempts = Arc::new(AtomicUsize::new(0));
    let caller = tokio::spawn({
        let cache = cache.clone();
        let key = key.clone();
        let started = started.clone();
        let finish = finish.clone();
        let attempts = attempts.clone();
        async move {
            cache
                .read(key, QuotaFreshness::Cached, move || async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    started.notify_one();
                    finish.notified().await;
                    Ok(usage(25.0))
                })
                .await
        }
    });
    started.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    finish.notify_one();
    let first = cache
        .read(key.clone(), QuotaFreshness::Cached, || async {
            panic!("a joined refresh must not fetch again")
        })
        .await
        .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        first,
        QuotaRead {
            usage: Some(usage(25.0)),
            captured_at: first.captured_at,
            error: None,
            retry_after_sec: 60,
        }
    );
    assert!(first.captured_at.is_some());

    let other = cache
        .read(
            QuotaKey {
                account: "bob".into(),
                ..key.clone()
            },
            QuotaFreshness::Cached,
            || async { Ok(usage(90.0)) },
        )
        .await
        .unwrap();
    assert_eq!(other.usage, Some(usage(90.0)));

    tokio::time::advance(Duration::from_secs(/*secs*/ 61)).await;
    let failed = cache
        .read(key.clone(), QuotaFreshness::Cached, || async {
            Err(QuotaFailure {
                message: "temporarily unavailable".into(),
                retry_after: RetryAfter::from_delay(Duration::from_secs(/*secs*/ 300)),
            })
        })
        .await
        .unwrap();
    assert_eq!(
        failed,
        QuotaRead {
            error: Some("temporarily unavailable".into()),
            retry_after_sec: 300,
            ..first.clone()
        }
    );
    let cooling = cache
        .read(key.clone(), QuotaFreshness::Refresh, || async {
            panic!("even an explicit refresh must respect a failed request's cooldown")
        })
        .await
        .unwrap();
    assert_eq!(cooling, failed);

    tokio::time::advance(Duration::from_secs(/*secs*/ 301)).await;
    let recovered = cache
        .read(key, QuotaFreshness::Cached, || async { Ok(usage(30.0)) })
        .await
        .unwrap();
    assert_eq!(
        recovered,
        QuotaRead {
            usage: Some(usage(30.0)),
            captured_at: recovered.captured_at,
            error: None,
            retry_after_sec: 60,
        }
    );
}
