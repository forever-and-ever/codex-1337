use super::CredentialProvider;
use super::CredentialSource;

const OPENAI_API_KEY_ENV_VARS: &[&str] = &["OPENAI_API_KEY"];
const OPENAI_API_KEY_PREFIXES: &[&str] = &["sk-proj-", "sk-svcacct-", "sk-admin-", "sk-"];
const OPENAI_BASE_URL_ENV_VAR: &str = "OPENAI_BASE_URL";
const OPENAI_API_KEY_MIN_LEN: usize = 51;

pub(super) static PROVIDER: CredentialProvider = CredentialProvider {
    context_env_vars: &[OPENAI_BASE_URL_ENV_VAR],
    credential_prefixes: OPENAI_API_KEY_PREFIXES,
    ignored_credential_prefixes: &["sk-ant-", "sk-or-"],
    credential_watermark: Some("T3BlbkFJ"),
    minimum_credential_len: OPENAI_API_KEY_MIN_LEN,
    sources: &[CredentialSource {
        env_vars: OPENAI_API_KEY_ENV_VARS,
        binding_env_vars: &[OPENAI_BASE_URL_ENV_VAR],
    }],
};
