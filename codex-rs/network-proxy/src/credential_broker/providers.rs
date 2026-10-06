mod github;
mod openai;

/// Metadata used to recognize built-in credentials and sanitize carried environments.
pub(super) struct CredentialProvider {
    pub(super) context_env_vars: &'static [&'static str],
    sources: &'static [CredentialSource],
}

pub(super) struct CredentialSource {
    pub(super) env_vars: &'static [&'static str],
    pub(super) binding_env_vars: &'static [&'static str],
}

const CREDENTIAL_PROVIDERS: &[&CredentialProvider] = &[&github::PROVIDER, &openai::PROVIDER];

impl CredentialProvider {
    pub(super) fn sources(&self) -> &[CredentialSource] {
        self.sources
    }
}

pub(super) fn credential_context_env_keys(
    brokered_keys: &[String],
) -> impl Iterator<Item = &'static str> + '_ {
    credential_providers()
        .filter(move |provider| {
            provider.sources().iter().any(|source| {
                source.env_vars.iter().any(|key| {
                    brokered_keys
                        .iter()
                        .any(|brokered_key| super::env_key_matches(brokered_key, key))
                })
            })
        })
        .flat_map(|provider| provider.context_env_vars.iter().copied())
}

pub(super) fn credential_env_keys() -> impl Iterator<Item = &'static str> {
    credential_providers()
        .flat_map(CredentialProvider::sources)
        .flat_map(|source| source.env_vars.iter().copied())
}

pub(super) fn credential_providers() -> impl Iterator<Item = &'static CredentialProvider> {
    CREDENTIAL_PROVIDERS.iter().copied()
}
