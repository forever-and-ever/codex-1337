use std::io;
use std::time::Duration;

use chrono::Utc;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClient;
use codex_http_client::HttpClientFactory;
use codex_http_client::RouteAwareClientPool;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::AccountId;
use super::AccountIdentity;
use super::AccountInfo;
use super::AccountStore;
use super::CLIENT_ID;
use super::OAUTH_SCOPE;
use super::SubscriptionGrant;
use super::TOKEN_URL;
use super::callback;
use crate::oauth::AuthorizationCodeGrant;
use crate::oauth::AuthorizationRequest;
use crate::oauth::ErrorBodyLimit;
use crate::oauth::OAuthClient;
use crate::oauth::PkceCodes;
use crate::oauth::TokenEncoding;
use crate::oauth::TokenEndpoint;
use crate::oauth::build_authorization_url;
use crate::oauth::generate_pkce;
use crate::oauth::generate_state;

const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const CALLBACK_PORT: u16 = 54545;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 300);
const PROFILE_RESPONSE_LIMIT: usize = 64 * 1024;

#[derive(Clone)]
pub struct LoginCancelHandle(watch::Sender<bool>);

impl LoginCancelHandle {
    pub fn cancel(&self) {
        self.0.send_replace(/*value*/ true);
    }
}

/// Owns one browser authorization attempt. Dropping it closes its callback listener.
/// The caller opens `authorization_url`; no browser or Claude Code installation is required here.
pub struct LoginAttempt {
    authorization_url: String,
    redirect_uri: String,
    state: String,
    pkce: PkceCodes,
    http: HttpClient,
    token_url: String,
    profile_url: String,
    deadline: Instant,
    cancel: LoginCancelHandle,
    cancelled: watch::Receiver<bool>,
    callback: Option<JoinHandle<io::Result<String>>>,
}

impl LoginAttempt {
    pub async fn start(factory: &HttpClientFactory) -> io::Result<Self> {
        Self::bind(
            factory,
            CALLBACK_PORT,
            TOKEN_URL,
            PROFILE_URL,
            LOGIN_TIMEOUT,
        )
        .await
    }

    async fn bind(
        factory: &HttpClientFactory,
        port: u16,
        token_url: &str,
        profile_url: &str,
        timeout: Duration,
    ) -> io::Result<Self> {
        let http = RouteAwareClientPool::new_without_redirects_or_request_logging(
            factory.clone(),
            ClientRouteClass::Auth,
        )
        .into_client();
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        // Use the established redirect registration; arbitrary ports are not assumed supported.
        let redirect_uri = format!(
            "http://localhost:{}/callback",
            listener.local_addr()?.port()
        );
        let pkce = generate_pkce();
        let state = generate_state();
        let authorization_url = build_authorization_url(AuthorizationRequest {
            endpoint: AUTHORIZE_URL,
            client_id: CLIENT_ID,
            redirect_uri: &redirect_uri,
            scope: Some(OAUTH_SCOPE),
            resource: None,
            pkce: &pkce,
            state: &state,
            extra_parameters: &[("code", "true")],
        })
        .map_err(io::Error::other)?
        .into();
        let deadline = Instant::now() + timeout;
        let (sender, cancelled) = watch::channel(/*init*/ false);
        let mut callback_cancelled = cancelled.clone();
        let callback_state = state.clone();
        let callback = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = callback_cancelled.wait_for(|cancelled| *cancelled) => Err(cancel_error()),
                _ = tokio::time::sleep_until(deadline) => Err(timeout_error()),
                result = callback::receive(listener, &callback_state) => result,
            }
        });
        Ok(Self {
            authorization_url,
            redirect_uri,
            state,
            pkce,
            http,
            token_url: token_url.to_owned(),
            profile_url: profile_url.to_owned(),
            deadline,
            cancel: LoginCancelHandle(sender),
            cancelled,
            callback: Some(callback),
        })
    }

    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    pub fn cancel_handle(&self) -> LoginCancelHandle {
        self.cancel.clone()
    }

    /// Adds or refreshes only the returned identity; never changes account selection.
    /// Cancellation applies until persistence starts. The atomic commit then runs to completion.
    pub async fn complete(mut self, store: &AccountStore) -> io::Result<AccountId> {
        let mut cancelled = self.cancelled.clone();
        let result = tokio::select! {
            biased;
            _ = cancelled.wait_for(|cancelled| *cancelled) => Err(cancel_error()),
            _ = tokio::time::sleep_until(self.deadline) => Err(timeout_error()),
            result = self.authorize() => result,
        };
        if let Some(callback) = self.callback.take() {
            callback.abort();
            let _ = callback.await;
        }
        let (info, grant) = result?;
        if *self.cancelled.borrow() {
            return Err(cancel_error());
        }
        if Instant::now() >= self.deadline {
            return Err(timeout_error());
        }
        store.upsert(info, grant).await
    }

    async fn authorize(&mut self) -> io::Result<(AccountInfo, SubscriptionGrant)> {
        let code = self
            .callback
            .as_mut()
            .ok_or_else(|| io::Error::other("Anthropic callback already consumed"))?
            .await
            .map_err(|_| io::Error::other("Anthropic callback task failed"));
        self.callback.take();
        let code = code??;
        let tokens: Tokens = OAuthClient::new(
            &self.http,
            TokenEndpoint {
                url: &self.token_url,
                client_id: CLIENT_ID,
                encoding: TokenEncoding::Json,
                timeout: Some(Duration::from_secs(/*secs*/ 30)),
                error_body_limit: ErrorBodyLimit::Bytes(8 * 1024),
            },
        )
        .exchange_code(AuthorizationCodeGrant {
            code: &code,
            redirect_uri: &self.redirect_uri,
            pkce: &self.pkce,
            resource: None,
            state: Some(&self.state),
        })
        .await
        .map_err(io::Error::other)?;
        let expires_at = Utc::now()
            .timestamp()
            .checked_add(tokens.expires_in)
            .filter(|_| tokens.expires_in > 0)
            .ok_or_else(|| io::Error::other("invalid Anthropic token expiry"))?;
        let grant = SubscriptionGrant::new(tokens.access_token, tokens.refresh_token, expires_at)?;
        let mut response = self
            .http
            .get(&self.profile_url)
            .bearer_auth(&grant.access_token)
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(/*secs*/ 30))
            .send()
            .await
            .map_err(|_| io::Error::other("Anthropic profile request failed"))?;
        if !response.status().is_success() {
            return Err(io::Error::other(format!(
                "Anthropic profile returned status {}",
                response.status()
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| io::Error::other("Anthropic profile body read failed"))?
        {
            if chunk.len() > PROFILE_RESPONSE_LIMIT.saturating_sub(body.len()) {
                return Err(io::Error::other("Anthropic profile response is too large"));
            }
            body.extend_from_slice(&chunk);
        }
        let profile: Profile = serde_json::from_slice(&body)
            .map_err(|_| io::Error::other("invalid Anthropic profile response"))?;
        Ok((
            AccountInfo {
                identity: AccountIdentity::new(&profile.account.uuid)?,
                email: profile.account.email,
                organization_uuid: profile.organization.as_ref().map(|org| org.uuid.clone()),
                organization_name: profile.organization.and_then(|org| org.name),
            },
            grant,
        ))
    }
}

impl Drop for LoginAttempt {
    fn drop(&mut self) {
        if let Some(callback) = &self.callback {
            callback.abort();
        }
    }
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

#[derive(Deserialize)]
struct Profile {
    account: ProfileAccount,
    organization: Option<ProfileOrganization>,
}

#[derive(Deserialize)]
struct ProfileAccount {
    uuid: String,
    email: Option<String>,
}

#[derive(Deserialize)]
struct ProfileOrganization {
    uuid: String,
    name: Option<String>,
}

fn cancel_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Anthropic login cancelled")
}

fn timeout_error() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "Anthropic login timed out")
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
