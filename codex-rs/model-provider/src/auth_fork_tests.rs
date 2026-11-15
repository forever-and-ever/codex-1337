use codex_agent_identity::generate_agent_key_material;
use codex_login::auth::AgentIdentityAuthRecord;
use codex_protocol::account::PlanType;
use http::header::AUTHORIZATION;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use super::*;

static NEXT_CODEX_HOME_ID: AtomicUsize = AtomicUsize::new(0);

async fn agent_identity_auth(chatgpt_account_is_fedramp: bool) -> AgentIdentityAuth {
    let key_material = generate_agent_key_material().expect("generate key material");
    AgentIdentityAuth::from_record(
        AgentIdentityAuthRecord {
            agent_runtime_id: "agent-runtime-1".to_string(),
            agent_private_key: key_material.private_key_pkcs8_base64,
            account_id: "account-1".to_string(),
            chatgpt_user_id: "user-1".to_string(),
            email: Some("agent@example.com".to_string()),
            plan_type: PlanType::Plus,
            chatgpt_account_is_fedramp,
            task_id: Some("task-run-1".to_string()),
        },
        "https://auth.openai.com/api/accounts",
        &codex_login::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("agent identity auth record should include task id")
}

fn provider_auth_scope(
    policy: AgentIdentityAuthPolicy,
    fallback: AgentIdentitySessionFallback,
) -> ProviderAuthScope {
    ProviderAuthScope {
        agent_identity_policy: policy,
        session_source: SessionSource::Cli,
        agent_identity_session_fallback: fallback,
    }
}

fn test_codex_home() -> PathBuf {
    let id = NEXT_CODEX_HOME_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "codex-model-provider-agent-identity-{pid}-{id}",
        pid = std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp codex home");
    path
}

#[test]
fn openai_provider_preserves_ambient_auth_headers() {
    let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    let mut expected = HeaderMap::new();
    expected.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer ambient-token"),
    );
    expected.insert(
        "ChatGPT-Account-ID",
        HeaderValue::from_static("account-123"),
    );
    let ambient_auth = CodexAuth::Headers(AuthHeaders::new(expected.clone()));

    let auth = resolve_provider_auth(Some(&ambient_auth), &provider).expect("auth should resolve");

    assert_eq!(auth.to_auth_headers().unwrap(), expected);
}

#[tokio::test]
async fn missing_conversation_account_rejects_request_setup() {
    let home = test_codex_home();
    let root =
        AuthManager::from_auth_for_testing_with_home(CodexAuth::from_api_key("test"), home.clone());
    let scoped = root.for_account("missing-account").await.unwrap();
    let result = resolve_provider_auth_for_scope(
        Some(scoped),
        /*auth*/ None,
        &ModelProviderInfo::create_openai_provider(/*base_url*/ None),
        provider_auth_scope(
            AgentIdentityAuthPolicy::JwtOnly,
            AgentIdentitySessionFallback::default(),
        ),
    )
    .await;
    let error = result
        .err()
        .expect("missing bound account must reject request setup");
    assert!(error.to_string().contains("/account"));
    std::fs::remove_dir_all(home).unwrap();
}

#[derive(Clone, Copy)]
enum RefreshFailure {
    Transient,
    Permanent,
    Policy,
}

struct FailingRefreshAuth {
    auth: tokio::sync::watch::Receiver<CodexAuth>,
    barrier: tokio::sync::Barrier,
    failure: RefreshFailure,
}

impl codex_login::ExternalAuth for FailingRefreshAuth {
    fn resolve(&self) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.auth.borrow().clone()) })
    }

    fn refresh(
        &self,
        _context: codex_login::ExternalAuthRefreshContext,
    ) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async {
            self.barrier.wait().await;
            self.barrier.wait().await;
            Err(std::io::Error::other("refresh service unavailable"))
        })
    }

    fn classify_error(&self, error: std::io::Error) -> codex_login::RefreshTokenError {
        match self.failure {
            RefreshFailure::Transient => codex_login::RefreshTokenError::Transient(error),
            RefreshFailure::Permanent => codex_login::auth::RefreshTokenFailedError::new(
                codex_login::auth::RefreshTokenFailedReason::Other,
                error.to_string(),
            )
            .into(),
            RefreshFailure::Policy => codex_login::RefreshTokenError::Policy(
                codex_http_client::NetworkPolicyDenied::Revoked,
            ),
        }
    }
}

#[tokio::test]
async fn failed_refresh_retries_only_with_newer_credentials() {
    for failure in [
        RefreshFailure::Transient,
        RefreshFailure::Permanent,
        RefreshFailure::Policy,
    ] {
        for (token, account, changed) in [
            ("Bearer old", "account", false),
            ("Bearer new", "account", true),
            ("Bearer new", "other-account", false),
        ] {
            let initial = CodexAuth::Headers(AuthHeaders::new(HeaderMap::from_iter([
                (AUTHORIZATION, HeaderValue::from_static("Bearer old")),
                (
                    http::HeaderName::from_static("chatgpt-account-id"),
                    HeaderValue::from_static("account"),
                ),
            ])));
            let (updates, auth) = tokio::sync::watch::channel(initial.clone());
            let external = Arc::new(FailingRefreshAuth {
                auth,
                barrier: tokio::sync::Barrier::new(2),
                failure,
            });
            let manager = AuthManager::from_auth_for_testing(initial.clone());
            manager.set_external_auth(external.clone()).await.unwrap();
            let provider = auth_provider_from_auth_manager(manager.clone(), &initial);
            let rejected = provider.to_auth_headers().expect("initial auth headers");
            let recovery = provider.refresh_after_unauthorized(rejected);
            let concurrent_update = async {
                external.barrier.wait().await;
                updates.send_replace(CodexAuth::Headers(AuthHeaders::new(HeaderMap::from_iter(
                    [
                        (AUTHORIZATION, HeaderValue::from_static(token)),
                        (
                            http::HeaderName::from_static("chatgpt-account-id"),
                            HeaderValue::from_static(account),
                        ),
                    ],
                ))));
                manager.reload().await;
                external.barrier.wait().await;
            };
            let (retry, ()) = tokio::join!(recovery, concurrent_update);
            assert_eq!(retry, changed && !matches!(failure, RefreshFailure::Policy));
            if matches!(failure, RefreshFailure::Permanent) && token == "Bearer old" {
                // Even a late rejection must not reuse a credential known to have failed.
                assert!(!provider.refresh_after_unauthorized(HeaderMap::new()).await);
            }
        }
    }
}

#[tokio::test]
async fn first_party_run_scope_uses_agent_assertion_and_exposes_telemetry() {
    let auth =
        CodexAuth::AgentIdentity(agent_identity_auth(/*chatgpt_account_is_fedramp*/ false).await);
    let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);

    let auth = resolve_provider_auth_for_scope(
        /*auth_manager*/ None,
        Some(&auth),
        &provider,
        provider_auth_scope(
            AgentIdentityAuthPolicy::JwtOnly,
            AgentIdentitySessionFallback::default(),
        ),
    )
    .await
    .expect("auth should resolve");

    assert_eq!(
        auth.agent_identity_telemetry,
        Some(AgentIdentityTelemetry {
            agent_id: "agent-runtime-1".to_string(),
            task_id: "task-run-1".to_string(),
        })
    );
    let headers = auth.auth.to_auth_headers().unwrap();
    assert!(
        headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("AgentAssertion "))
    );
}
