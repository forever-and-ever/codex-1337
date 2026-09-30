//! Credential configuration and sanitation for carried managed-network metadata.
mod configured;
mod destination;
mod environment;
mod matching;
mod provider_config;
mod providers;

use std::collections::HashMap;

pub use environment::CredentialBrokerContext;
pub use environment::CredentialBrokerEnvironment;
pub use environment::brokered_credential_binding_env_keys;
pub use environment::brokered_credential_dummy_env_keys;
pub use environment::brokered_credential_env_keys;
pub use environment::brokered_credential_marker_env_keys;
pub use environment::brokered_credential_value_env_keys;
pub use environment::credential_broker_provider_context_env_keys;
pub use environment::credential_broker_provider_sources_allowed;
pub use environment::is_credential_broker_provider_env_key;
pub(crate) use environment::marked_credential_dummy_env_keys;
pub use provider_config::CredentialAuthMethod;
pub use provider_config::CredentialProviderConfig;

pub const CREDENTIAL_BROKER_ACTIVE_ENV_KEY: &str = "CODEX_NETWORK_PROXY_CREDENTIAL_BROKER_ACTIVE";
pub(crate) const BROKERED_CREDENTIALS_ENV_KEY: &str = "CODEX_NETWORK_PROXY_BROKERED_CREDENTIALS";
const MIN_EMBEDDED_CREDENTIAL_LENGTH: usize = 16;
const BROKERED_CREDENTIAL_ALIAS_MARKER_PREFIX: &str = "@alias:";

fn env_key_matches(candidate: &str, expected: &str) -> bool {
    if cfg!(windows) {
        candidate.eq_ignore_ascii_case(expected)
    } else {
        candidate == expected
    }
}

fn env_entry<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<(&'a str, &'a str)> {
    env.iter()
        .find(|(candidate, _)| env_key_matches(candidate, key))
        .map(|(key, value)| (key.as_str(), value.as_str()))
}

pub(super) fn env_value<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env_entry(env, key).map(|(_, value)| value)
}

fn set_env_value(env: &mut HashMap<String, String>, key: &str, value: String) {
    if cfg!(windows) {
        env.retain(|candidate, _| !env_key_matches(candidate, key));
    }
    env.insert(key.to_string(), value);
}

#[cfg(test)]
#[path = "credential_broker_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "credential_broker/configured_tests.rs"]
mod configured_tests;
