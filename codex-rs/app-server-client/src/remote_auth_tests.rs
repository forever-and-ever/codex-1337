use super::*;
use pretty_assertions::assert_eq;

#[test]
fn env_token_relaunch_uses_only_the_original_still_current_source() {
    let source = "CODEX_TEST_REMOTE_AUTH";
    let secret = "dummy-token";
    let token = RemoteAuthToken::from_env_with(source.to_string(), |name| {
        assert_eq!(name, source);
        Ok(format!("  {secret}  "))
    })
    .expect("env var should parse");
    assert_eq!(token.as_str(), secret);

    for (value, expected) in [
        (Ok(format!("\t{secret}\n")), Some(source)),
        (Ok("replacement".to_string()), None),
        (Ok(" \n\t ".to_string()), None),
        (Err(env::VarError::NotPresent), None),
    ] {
        assert_eq!(
            token.source_env_if_current_with(|name| {
                assert_eq!(name, source);
                value
            }),
            expected
        );
    }
    assert_eq!(
        RemoteAuthToken::from_secret(secret.to_string())
            .source_env_if_current_with(|_| panic!("ad-hoc tokens have no provenance")),
        None
    );
    let endpoint = crate::RemoteAppServerEndpoint::WebSocket {
        websocket_url: "wss://example.test/".to_string(),
        auth_token: Some(token),
    };
    let debug = format!("{endpoint:?}");
    assert!(!debug.contains(secret));
    assert!(!debug.contains(source));
}

#[test]
fn remote_auth_env_errors_distinguish_missing_and_empty_values() {
    for (result, detail) in [
        (Err(env::VarError::NotPresent), "is not set"),
        (Ok(" \n\t ".to_string()), "is empty"),
    ] {
        let err = RemoteAuthToken::from_env_with("CODEX_TEST_REMOTE_AUTH".to_string(), |_| result)
            .expect_err("unusable env vars should be rejected");
        assert!(err.to_string().contains(detail));
    }
}
