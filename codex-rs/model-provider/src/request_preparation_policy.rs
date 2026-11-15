//! Captured account-policy binding and same-source renewal.

use std::io;

use codex_login::AuthChangeState;
use codex_login::CapturedAuthSource;
use codex_protocol::error::Result;

use crate::ModelProvider;

pub(super) fn capture_runtime_source(provider: &dyn ModelProvider) -> Option<CapturedAuthSource> {
    provider
        .auth_manager()
        .and_then(|manager| manager.capture_runtime_source().ok())
}

// An opaque source owns bearer renewal, not a named account. Keep its exact
// source admission while rebuilding the request snapshot.
pub(super) fn permits_source_renewal(
    before: Option<AuthChangeState>,
    after: Option<AuthChangeState>,
    source: Option<&CapturedAuthSource>,
) -> bool {
    matches!(
        (before, after, source),
        (Some(before_state), Some(after_state), Some(source))
            if before_state.source_generation == after_state.source_generation
                && before_state.login_generation == after_state.login_generation
                && source.validate().is_ok()
    )
}

/// Retain account revocation even when destination policy suppresses runtime headers.
pub(crate) fn bind_request_policy(
    provider: &dyn ModelProvider,
    factory: codex_http_client::HttpClientFactory,
) -> Result<codex_http_client::HttpClientFactory> {
    let credentials = provider
        .auth_manager()
        .map(|manager| manager.http_client_factory());
    let policy = match credentials {
        Some(credentials) if credentials.network_policy().is_managed() => factory
            .network_policy()
            .clone()
            .with_account_binding(credentials.network_policy())
            .ok_or_else(|| io::Error::other("credential owner and transport policy differ"))?,
        _ => factory.network_policy().clone().for_current_account(),
    };
    Ok(factory.with_network_policy(policy))
}
