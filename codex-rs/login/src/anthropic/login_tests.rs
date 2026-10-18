use base64::Engine;
use codex_http_client::OutboundProxyPolicy;
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use url::Url;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;

async fn attempt(server: &MockServer, timeout: Duration) -> LoginAttempt {
    LoginAttempt::bind(
        &HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        /*port*/ 0,
        &format!("{}/token", server.uri()),
        &format!("{}/profile", server.uri()),
        timeout,
    )
    .await
    .unwrap()
}

async fn callback(attempt: &LoginAttempt, target: &str) {
    let redirect = Url::parse(&attempt.redirect_uri).unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
        .await
        .unwrap();
    stream
        .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
}

async fn endpoints(server: &MockServer, account: &str, access: &str) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "access_token": access, "refresh_token": "synthetic-refresh", "expires_in": 3600
        })))
        .expect(/*r*/ 1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/profile"))
        .and(header("authorization", format!("Bearer {access}")))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "account": {"uuid": account, "email": "example@example.test"},
            "organization": {"uuid": "org", "name": "Synthetic"}
        })))
        .expect(/*r*/ 1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn additive_login_binds_pkce_state_and_preserves_other_accounts() {
    let home = TempDir::new().unwrap();
    let store = AccountStore::new(home.path().to_owned());
    let other_info = AccountInfo {
        identity: AccountIdentity::new("existing-account").unwrap(),
        email: None,
        organization_uuid: None,
        organization_name: None,
    };
    let other = store
        .upsert(
            other_info.clone(),
            SubscriptionGrant::bearer_only("existing-bearer".into()).unwrap(),
        )
        .await
        .unwrap();
    let before = store.load(&other).unwrap();
    let server = MockServer::start().await;
    endpoints(&server, "new-account", "synthetic-access").await;
    let login = attempt(&server, LOGIN_TIMEOUT).await;
    let auth = Url::parse(login.authorization_url()).unwrap();
    let query = auth
        .query_pairs()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(query.get("scope").unwrap(), OAUTH_SCOPE);
    // Unrelated probes cannot terminate the attempt or advance it to token exchange.
    for probe in [
        "/callback?state=wrong&code=intruder".to_owned(),
        format!(
            "/callback?state={}&state={}&code=intruder",
            login.state, login.state
        ),
        format!("/elsewhere?state={}&code=intruder", login.state),
    ] {
        callback(&login, &probe).await;
    }
    callback(
        &login,
        &format!("/callback?state={}&code=synthetic-code", login.state),
    )
    .await;
    let id = login.complete(&store).await.unwrap();
    assert_eq!(id, AccountIdentity::new("new-account").unwrap().id());
    assert!(store.load(&other).unwrap() == before);
    assert_eq!(store.list().unwrap().len(), 2);
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = requests[0].body_json().unwrap();
    let verifier = body["code_verifier"].as_str().unwrap();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(query.get("code_challenge").unwrap().as_ref(), challenge);
    assert_eq!(
        body,
        json!({
            "grant_type": "authorization_code", "client_id": CLIENT_ID,
            "code": "synthetic-code", "state": query.get("state").unwrap(),
            "redirect_uri": query.get("redirect_uri").unwrap(), "code_verifier": verifier,
        })
    );
    server.reset().await;
    endpoints(&server, "new-account", "replacement-access").await;
    let login = attempt(&server, LOGIN_TIMEOUT).await;
    callback(
        &login,
        &format!("/callback?state={}&code=next-code", login.state),
    )
    .await;
    assert_eq!(login.complete(&store).await.unwrap(), id);
    assert_eq!(
        store.load(&id).unwrap().grant.access_token,
        "replacement-access"
    );
    assert!(store.load(&other).unwrap() == before);
    assert_eq!(store.list().unwrap().len(), 2);
}

#[tokio::test]
async fn cancellation_during_exchange_drops_network_and_preserves_store() {
    let home = TempDir::new().unwrap();
    let store = AccountStore::new(home.path().to_owned());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(/*s*/ 200)
                .set_delay(Duration::from_secs(/*secs*/ 30))
                .set_body_json(
                    json!({"access_token":"secret","refresh_token":"secret","expires_in":3600}),
                ),
        )
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let login = attempt(&server, LOGIN_TIMEOUT).await;
    callback(
        &login,
        &format!("/callback?state={}&code=synthetic-code", login.state),
    )
    .await;
    let cancel = login.cancel_handle();
    let complete = login.complete(&store);
    let cancel_after_exchange = async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(/*millis*/ 5)).await;
        }
        cancel.cancel();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(/*secs*/ 2), async {
        tokio::join!(complete, cancel_after_exchange)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn timeout_and_profile_failure_never_create_account_files() {
    let home = TempDir::new().unwrap();
    let store = AccountStore::new(home.path().to_owned());
    let server = MockServer::start().await;
    let login = attempt(&server, Duration::from_millis(/*millis*/ 10)).await;
    let port = Url::parse(&login.redirect_uri).unwrap().port().unwrap();
    assert_eq!(
        login.complete(&store).await.unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    drop(listener);
    let tokens = ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
        "access_token": "synthetic-access", "refresh_token": "synthetic-refresh", "expires_in": 3600
    }));
    let invalid_profile =
        ResponseTemplate::new(/*s*/ 200).set_body_json(json!({"account":{"uuid":""}}));
    let oversized = ResponseTemplate::new(/*s*/ 200).set_body_string("x".repeat(/*n*/ 1_048_577));
    let redirect = ResponseTemplate::new(/*s*/ 307)
        .insert_header("Location", format!("{}/sink", server.uri()));
    for (token_response, profile_response) in [
        (tokens.clone(), invalid_profile.clone()),
        (tokens.clone(), oversized.clone()),
        (oversized, invalid_profile.clone()),
        (redirect.clone(), invalid_profile),
        (tokens, redirect),
    ] {
        server.reset().await;
        Mock::given(path("/token"))
            .respond_with(token_response)
            .mount(&server)
            .await;
        Mock::given(path("/profile"))
            .respond_with(profile_response)
            .mount(&server)
            .await;
        let login = attempt(&server, LOGIN_TIMEOUT).await;
        callback(
            &login,
            &format!("/callback?state={}&code=synthetic-code", login.state),
        )
        .await;
        assert!(login.complete(&store).await.is_err());
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| request.url.path() != "/sink")
        );
    }
    server.reset().await;
    let login = attempt(&server, LOGIN_TIMEOUT).await;
    let state = login.state.clone();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(/*s*/ 400).set_body_json(json!({
            "error":"invalid_grant", "error_description":format!("invalid state {state}")
        })))
        .mount(&server)
        .await;
    callback(
        &login,
        &format!("/callback?state={state}&code=synthetic-code"),
    )
    .await;
    let error = login.complete(&store).await.unwrap_err();
    assert!(!format!("{error:?} {error}").contains(&state));
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}
