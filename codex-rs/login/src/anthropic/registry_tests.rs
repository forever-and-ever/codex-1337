use std::time::Duration;

use chrono::Utc;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::method;

use super::*;
use crate::test_support::transport_default_auth_route_config;

fn info(account: &str) -> AccountInfo {
    AccountInfo {
        identity: AccountIdentity::new(account).unwrap(),
        email: Some(format!("{account}@example.test")),
        organization_uuid: Some("organization".to_owned()),
        organization_name: None,
    }
}

fn grant(access: &str, refresh: &str, expires_at: i64) -> SubscriptionGrant {
    SubscriptionGrant::new(access.to_owned(), refresh.to_owned(), expires_at).unwrap()
}

fn manager(store: &AccountStore, server: &MockServer) -> AuthManager {
    AuthManager::with_endpoint(
        store.clone(),
        transport_default_auth_route_config().http_client_factory(),
        server.uri(),
    )
    .unwrap()
}

async fn refresh_mock(server: &MockServer, refresh: &str, template: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(body_json(json!({
            "client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": refresh,
            "scope": OAUTH_SCOPE,
        })))
        .respond_with(template)
        .expect(/*r*/ 1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn account_local_refresh_survives_cancellation_and_deduplicates_across_managers() {
    // Rotation consumes a grant even if its inference caller is cancelled. A second process
    // must reread the persisted replacement, while another account must never wait on A.
    let home = tempfile::tempdir().unwrap();
    let store = AccountStore::new(home.path().to_path_buf());
    let server = MockServer::start().await;
    let a_info = info("account-a");
    let b_info = info("account-b");
    let a = store
        .upsert(
            a_info.clone(),
            grant("a-old", "a-refresh", /*expires_at*/ 1),
        )
        .await
        .unwrap();
    let b = store
        .upsert(
            b_info.clone(),
            grant("b-old", "b-refresh", /*expires_at*/ 1),
        )
        .await
        .unwrap();
    refresh_mock(
        &server,
        "a-refresh",
        ResponseTemplate::new(/*s*/ 200)
            .set_body_json(
                json!({"access_token":"a-new","refresh_token":"a-rotated","expires_in":3600}),
            )
            .set_delay(Duration::from_millis(/*millis*/ 100)),
    )
    .await;
    refresh_mock(
        &server,
        "b-refresh",
        ResponseTemplate::new(/*s*/ 200).set_body_json(
            json!({"access_token":"b-new","refresh_token":"b-rotated","expires_in":3600}),
        ),
    )
    .await;
    let gate = store.lock(&a).await.unwrap();
    let a_handle = manager(&store, &server).pin(a.clone());
    let task = tokio::spawn(async move { a_handle.credentials().await });
    let b_credentials = manager(&store, &server)
        .pin(b.clone())
        .credentials()
        .await
        .unwrap();
    assert_eq!(b_credentials.access_token(), "b-new");
    drop(gate);
    tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
        loop {
            if server.received_requests().await.unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 5)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    let a_credentials = manager(&store, &server)
        .pin(a.clone())
        .credentials()
        .await
        .unwrap();
    assert_eq!(a_credentials.access_token(), "a-new");
    assert_eq!(
        a_credentials,
        Credentials {
            account_id: a.clone(),
            grant: store.load(&a).unwrap().grant
        }
    );
    assert_eq!(
        store.load(&b).unwrap().grant.refresh_token.as_deref(),
        Some("b-rotated")
    );
    let mut expected = vec![a_info, b_info];
    expected.sort_by_key(|info| info.identity.id());
    assert_eq!(store.list().unwrap(), expected);
    assert!(!format!("{a_credentials:?}").contains("a-new"));
}

#[tokio::test]
async fn failed_refresh_is_account_local_and_native_login_supersedes_its_grant() {
    let home = tempfile::tempdir().unwrap();
    let store = AccountStore::new(home.path().to_path_buf());
    let server = MockServer::start().await;
    let c = store
        .upsert(
            info("account-c"),
            SubscriptionGrant::bearer_only("c-bearer".into()).unwrap(),
        )
        .await
        .unwrap();
    let bearer_handle = manager(&store, &server).pin(c);
    let bearer = bearer_handle.credentials().await.unwrap();
    assert_eq!(bearer.expires_at(), None);
    assert!(matches!(
        bearer_handle.refresh_after_rejection(&bearer).await,
        Err(AuthError::LoginRequired(required)) if required.account == "account-c@example.test" && required.provider == codex_protocol::sign_in_required::AccountProvider::ClaudeSubscription
    ));
    let a = store
        .upsert(
            info("account-a"),
            grant("a-original", "a-refresh", Utc::now().timestamp() + 3600),
        )
        .await
        .unwrap();
    let handle = manager(&store, &server).pin(a.clone());
    let captured = handle.credentials().await.unwrap();
    let b = store
        .upsert(
            info("account-b"),
            grant("b-old", "b-refresh", /*expires_at*/ 1),
        )
        .await
        .unwrap();
    let before = store.load(&b).unwrap();
    refresh_mock(
        &server,
        "b-refresh",
        ResponseTemplate::new(/*s*/ 429)
            .set_body_json(json!({"error":"rate_limited", "description":"b-refresh a-original"})),
    )
    .await;
    let error = manager(&store, &server)
        .pin(b.clone())
        .credentials()
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::Rejected(429)));
    assert!(!format!("{error:?} {error}").contains("b-refresh"));
    assert!(store.load(&b).unwrap() == before);
    assert_eq!(handle.credentials().await.unwrap(), captured);
    refresh_mock(
        &server,
        "a-refresh",
        ResponseTemplate::new(/*s*/ 200).set_body_json(
            json!({"access_token":"a-new","refresh_token":"a-rotated","expires_in":3600,
            "account":{"uuid":"account-a"},"organization":{"uuid":"organization"}}),
        ),
    )
    .await;
    let renewed = handle.refresh_after_rejection(&captured).await.unwrap();
    assert_eq!(renewed.account_id(), &a);
    assert_eq!(renewed.access_token(), "a-new");
    store
        .upsert(
            info("account-a"),
            grant("a-login", "a-login-refresh", Utc::now().timestamp() + 3600),
        )
        .await
        .unwrap();
    assert_eq!(
        handle.credentials().await.unwrap().access_token(),
        "a-login"
    );
    assert!(store.load(&b).unwrap() == before);
    // A dead grant is recoverable identity, not an arbitrary endpoint failure.
    server.reset().await;
    for (status, body) in [
        (400, json!({"error": "invalid_grant"})),
        (401, json!({"error": "invalid_token"})),
        (403, json!({"error": "access_denied"})),
    ] {
        refresh_mock(
            &server,
            "b-refresh",
            ResponseTemplate::new(status).set_body_json(body),
        )
        .await;
        let error = manager(&store, &server)
            .pin(b.clone())
            .credentials()
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::LoginRequired(ref required)
            if required.account == "account-b@example.test"));
        server.reset().await;
    }
    server.verify().await;
}
