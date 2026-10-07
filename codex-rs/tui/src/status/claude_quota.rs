use super::super::helpers::format_reset_timestamp;
use super::super::rate_limits::RateLimitSnapshotDisplay;
use super::super::rate_limits::RateLimitWindowDisplay;
use super::super::rate_limits::StatusRateLimitData;
use super::super::rate_limits::compose_rate_limit_data;
use super::StatusHistoryHandle;
use crate::clock_format::ClockFormat;
use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use codex_app_server_protocol::AccountQuota;

pub(crate) fn claude_quota_data(
    quota: Option<&AccountQuota>,
    now: DateTime<Local>,
    clock_format: ClockFormat,
) -> StatusRateLimitData {
    let Some(quota) = quota else {
        return StatusRateLimitData::Unavailable;
    };
    let captured_at = quota
        .captured_at
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
        .map(|timestamp| timestamp.with_timezone(&Local));
    let windows = [(&quota.five_hour, 300), (&quota.seven_day, 10_080)].map(|(window, minutes)| {
        window.as_ref().map(|window| RateLimitWindowDisplay {
            used_percent: (100.0 - window.remaining_percent).clamp(0.0, 100.0),
            resets_at: window
                .resets_at
                .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
                .map(|timestamp| {
                    format_reset_timestamp(
                        timestamp.with_timezone(&Local),
                        captured_at.unwrap_or(now),
                        clock_format,
                    )
                }),
            window_minutes: Some(minutes),
        })
    });
    let [primary, secondary] = windows;
    let data = compose_rate_limit_data(
        Some(&RateLimitSnapshotDisplay {
            limit_name: "codex".to_string(),
            normal_model_slug: None,
            captured_at: captured_at.unwrap_or(now),
            primary,
            secondary,
            credits: None,
            individual_limit: None,
        }),
        now,
    );
    match data {
        StatusRateLimitData::Available(rows) if captured_at.is_none() || quota.error.is_some() => {
            StatusRateLimitData::Stale(rows)
        }
        data => data,
    }
}

impl StatusHistoryHandle {
    pub(crate) fn finish_claude_quota_refresh(&self, data: StatusRateLimitData) {
        let mut state = self
            .card
            .rate_limit_state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.rate_limits = data;
        state.refreshing_rate_limits = false;
    }
}

#[cfg(test)]
#[path = "claude_quota_tests.rs"]
mod tests;
