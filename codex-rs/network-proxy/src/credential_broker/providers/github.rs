use super::CredentialProvider;
use super::CredentialSource;

const GH_HOST_ENV_VAR: &str = "GH_HOST";
const GITHUB_CLOUD_TOKEN_ENV_VARS: &[&str] = &["GH_TOKEN", "GITHUB_TOKEN"];
const GITHUB_ENTERPRISE_TOKEN_ENV_VARS: &[&str] =
    &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"];

pub(super) static PROVIDER: CredentialProvider = CredentialProvider {
    context_env_vars: &[GH_HOST_ENV_VAR],
    sources: &[
        CredentialSource {
            env_vars: GITHUB_CLOUD_TOKEN_ENV_VARS,
            binding_env_vars: &[],
        },
        CredentialSource {
            env_vars: GITHUB_ENTERPRISE_TOKEN_ENV_VARS,
            binding_env_vars: &[GH_HOST_ENV_VAR],
        },
    ],
};
