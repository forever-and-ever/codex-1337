use super::CredentialProviderConfig;

#[test]
fn configured_provider_rejects_unsafe_destinations_and_unusable_dummy_patterns() {
    let impossible_assertion = CredentialProviderConfig {
        env: vec!["PROVIDER_PASSWORD".to_string()],
        patterns: vec![r"^pass_\b[a-z]{24}$".to_string()],
        url_prefixes: vec!["api.example".to_string()],
        ..CredentialProviderConfig::default()
    };
    assert!(impossible_assertion.validate("custom").is_err());

    for (pattern, url_prefixes) in [
        (r"\b|token_[a-z]{24}", vec!["api.example"]),
        ("provider_[a-z]{24}", vec!["*"]),
    ] {
        let provider = CredentialProviderConfig {
            env: vec!["FIRST_TOKEN".to_string(), "SECOND_TOKEN".to_string()],
            patterns: vec![pattern.to_string()],
            url_prefixes: url_prefixes.into_iter().map(str::to_string).collect(),
            ..CredentialProviderConfig::default()
        };
        assert!(provider.validate("custom").is_err());
    }
}
