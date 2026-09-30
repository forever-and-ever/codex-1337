use super::*;

use pretty_assertions::assert_eq;

fn env_map<const N: usize>(entries: [(&str, &str); N]) -> HashMap<String, String> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn credential_provider_sources_respect_environment_ownership() {
    for key in [
        "sk-ant-api03-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmno",
        "sk-ant-oat01-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmno",
        "sk-or-v1-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmno",
    ] {
        assert!(!credential_broker_provider_sources_allowed(
            key,
            key,
            &HashMap::new(),
            |_| true,
        ));
    }
    let unknown_github_token = "ghp_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefgh";
    let unknown_legacy_openai_key = format!("sk-{}", "a".repeat(48));
    let unrelated_collision = format!("sk-ant-api03-{}-sk-{}", "a".repeat(40), "b".repeat(48));
    let redacted_collision = format!("sk-ant-api03-{}-", "a".repeat(40));
    assert!(credential_broker_provider_sources_allowed(
        &unrelated_collision,
        &redacted_collision,
        &HashMap::new(),
        |_| true,
    ));
    assert!(!credential_broker_provider_sources_allowed(
        &format!("{unknown_github_token}_{unrelated_collision}"),
        &format!("_{redacted_collision}"),
        &HashMap::new(),
        |source| source != "OPENAI_API_KEY",
    ));
    let unrelated = "sk-ant-api03-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmno";
    assert!(!credential_broker_provider_sources_allowed(
        &format!("{unknown_github_token}_{unknown_legacy_openai_key}_{unrelated}"),
        &format!("__{unrelated}"),
        &HashMap::new(),
        |source| source != "OPENAI_API_KEY",
    ));
    let equivalent_sources = env_map([
        ("GH_TOKEN", unknown_github_token),
        ("GITHUB_TOKEN", unknown_github_token),
    ]);
    assert!(credential_broker_provider_sources_allowed(
        unknown_github_token,
        "",
        &equivalent_sources,
        |source| source == "GH_TOKEN",
    ));
    let distinct_github_token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH";
    let distinct_sources = env_map([
        ("GH_TOKEN", unknown_github_token),
        ("GITHUB_TOKEN", distinct_github_token),
    ]);
    assert!(!credential_broker_provider_sources_allowed(
        &format!("{unknown_github_token}\n{distinct_github_token}"),
        "",
        &distinct_sources,
        |source| source == "GH_TOKEN",
    ));
}

#[test]
fn brokered_credential_env_keys_only_include_registered_credentials() {
    let mut env = env_map([
        ("OPENAI_API_KEY", "sk-real"),
        ("GH_TOKEN", ""),
        ("GH_HOST", "github.example.com"),
    ]);

    env.insert(
        CREDENTIAL_BROKER_ACTIVE_ENV_KEY.to_string(),
        "1".to_string(),
    );
    env.insert(
        BROKERED_CREDENTIALS_ENV_KEY.to_string(),
        serde_json::json!([["OPENAI_API_KEY", "sk-real"]]).to_string(),
    );
    env.insert(
        "GH_TOKEN".to_string(),
        "ghp_added_after_brokerage".to_string(),
    );

    assert_eq!(
        brokered_credential_env_keys(&env).collect::<Vec<_>>(),
        vec!["OPENAI_API_KEY"]
    );
}

#[test]
fn brokered_credential_value_env_keys_include_dummy_aliases() {
    let dummy = "sk-proj-abcdefghijklmnopqrstuvwxyz";
    let alias = format!("Bearer {dummy}");
    let mut env = env_map([("OPENAI_API_KEY", dummy), ("AUTH_HEADER", alias.as_str())]);

    env.insert(
        CREDENTIAL_BROKER_ACTIVE_ENV_KEY.to_string(),
        "1".to_string(),
    );
    env.insert(
        BROKERED_CREDENTIALS_ENV_KEY.to_string(),
        serde_json::json!([
            ["@alias:AUTH_HEADER", dummy],
            ["@alias:OPENAI_API_KEY", dummy],
            ["OPENAI_API_KEY", dummy],
        ])
        .to_string(),
    );

    assert_eq!(
        brokered_credential_value_env_keys(&env),
        vec!["AUTH_HEADER".to_string(), "OPENAI_API_KEY".to_string()]
    );

    let mut absent_alias_env = env.clone();
    absent_alias_env.remove("AUTH_HEADER");
    assert_eq!(
        brokered_credential_marker_env_keys(&absent_alias_env),
        vec!["AUTH_HEADER".to_string(), "OPENAI_API_KEY".to_string()]
    );

    env.remove("OPENAI_API_KEY");
    assert!(brokered_credential_dummy_env_keys(&env).is_empty());
    assert_eq!(
        brokered_credential_marker_env_keys(&env),
        vec!["AUTH_HEADER".to_string(), "OPENAI_API_KEY".to_string()]
    );
    assert_eq!(
        brokered_credential_value_env_keys(&env),
        vec!["AUTH_HEADER".to_string()]
    );
}
