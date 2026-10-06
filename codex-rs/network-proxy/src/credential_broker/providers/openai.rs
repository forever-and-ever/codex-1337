use super::CredentialProvider;
use super::CredentialSource;

const OPENAI_API_KEY_ENV_VARS: &[&str] = &["OPENAI_API_KEY"];
const OPENAI_BASE_URL_ENV_VAR: &str = "OPENAI_BASE_URL";

pub(super) static PROVIDER: CredentialProvider = CredentialProvider {
    context_env_vars: &[OPENAI_BASE_URL_ENV_VAR],
    sources: &[CredentialSource {
        env_vars: OPENAI_API_KEY_ENV_VARS,
        binding_env_vars: &[OPENAI_BASE_URL_ENV_VAR],
    }],
};
