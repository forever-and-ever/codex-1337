use super::*;

use pretty_assertions::assert_eq;

fn env_map<const N: usize>(entries: [(&str, &str); N]) -> HashMap<String, String> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
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
