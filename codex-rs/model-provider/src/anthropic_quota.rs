//! Native subscription usage transport. The server owns polling and shared cooldowns.

use chrono::DateTime;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientBuilder;
use codex_http_client::RetryAfter;
use codex_login::AuthRuntimeConfig;
use codex_login::anthropic::AccountId;
use codex_protocol::protocol::RateLimitWindow;
use http::HeaderValue;
use serde::Deserialize;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

#[derive(Clone, Debug, PartialEq)]
pub struct AnthropicQuota {
    pub five_hour: Option<RateLimitWindow>,
    pub seven_day: Option<RateLimitWindow>,
}

#[derive(Debug)]
pub struct AnthropicQuotaError {
    pub message: String,
    pub retry_after: Option<RetryAfter>,
}

fn failure(message: impl Into<String>) -> AnthropicQuotaError {
    AnthropicQuotaError {
        message: message.into(),
        retry_after: None,
    }
}

/// Uses the same pinned account and refresh owner as native inference; never selects an account.
/// The caller bounds the complete operation, including waiting for credential refresh.
pub async fn read_anthropic_quota(
    runtime: &AuthRuntimeConfig,
    id: AccountId,
) -> Result<AnthropicQuota, AnthropicQuotaError> {
    let auth = crate::shared_state::process_shared_state()
        .anthropic_auth(runtime)
        .map_err(|_| failure("Claude account registry unavailable"))?;
    let http = HttpClientBuilder::new()
        .without_redirects()
        .without_request_logging()
        .build_respecting_outbound_proxy_policy(
            runtime.auth_route_config.http_client_factory(),
            USAGE_URL,
            ClientRouteClass::Auth,
        )
        .map_err(|_| failure("Claude usage HTTP client unavailable"))?;
    let account = auth.pin(id);
    let mut credentials = account
        .credentials()
        .await
        .map_err(|_| failure("Claude account credentials unavailable"))?;
    for attempt in 0..2 {
        let mut bearer = HeaderValue::from_str(&format!("Bearer {}", credentials.access_token()))
            .map_err(|_| failure("Invalid Claude bearer"))?;
        bearer.set_sensitive(true);
        let mut response = http
            .get(USAGE_URL)
            .header(http::header::AUTHORIZATION, bearer)
            .header("anthropic-beta", "oauth-2025-04-20")
            .send()
            .await
            .map_err(|_| failure("Claude quota transport failed"))?;
        if response.status() == http::StatusCode::UNAUTHORIZED && attempt == 0 {
            credentials = account
                .refresh_after_rejection(&credentials)
                .await
                .map_err(|_| failure("Claude account refresh failed"))?;
            continue;
        }
        if !response.status().is_success() {
            return Err(AnthropicQuotaError {
                message: format!(
                    "Claude quota unavailable (HTTP {})",
                    response.status().as_u16()
                ),
                retry_after: RetryAfter::from_headers(response.headers()),
            });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| failure("Claude quota body failed"))?
        {
            if body.len() + chunk.len() > 64 * 1024 {
                return Err(failure("Claude quota response is too large"));
            }
            body.extend_from_slice(&chunk);
        }
        let usage: Usage =
            serde_json::from_slice(&body).map_err(|_| failure("Invalid Claude quota response"))?;
        return usage.project();
    }
    unreachable!()
}

#[derive(Deserialize)]
struct Usage {
    five_hour: Option<Window>,
    seven_day: Option<Window>,
}

#[derive(Deserialize)]
struct Window {
    utilization: f64,
    resets_at: Option<String>,
}

impl Usage {
    fn project(self) -> Result<AnthropicQuota, AnthropicQuotaError> {
        if self.five_hour.is_none() && self.seven_day.is_none() {
            return Err(failure("Claude quota response contains no windows"));
        }
        let window = |window: Window, minutes| {
            if !window.utilization.is_finite() || window.utilization < 0.0 {
                return Err(failure("Invalid Claude quota utilization"));
            }
            Ok(RateLimitWindow {
                used_percent: window.utilization,
                window_minutes: Some(minutes),
                resets_at: window
                    .resets_at
                    .map(|timestamp| {
                        DateTime::parse_from_rfc3339(&timestamp)
                            .map(|time| time.timestamp())
                            .map_err(|_| failure("Invalid Claude quota reset time"))
                    })
                    .transpose()?,
            })
        };
        Ok(AnthropicQuota {
            five_hour: self.five_hour.map(|value| window(value, 300)).transpose()?,
            seven_day: self
                .seven_day
                .map(|value| window(value, 10080))
                .transpose()?,
        })
    }
}
