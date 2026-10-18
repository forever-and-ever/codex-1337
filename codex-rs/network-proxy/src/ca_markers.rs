//! Recognizes inherited managed CA bundles without creating TLS state.
use anyhow::Context as _;
use anyhow::Result;
use codex_utils_home_dir::find_codex_home;
use sha2::Digest as _;
use sha2::Sha256;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
const MANAGED_MITM_CA_DIR: &str = "proxy";
const MANAGED_MITM_EXTERNAL_CA_DIR: &str = "external";
const MANAGED_MITM_CA_TRUST_BUNDLE_PREFIX: &str = "ca-bundle";
pub const CUSTOM_CA_ENV_KEYS: [&str; 11] = [
    "CODEX_CA_CERTIFICATE",
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "NODE_EXTRA_CA_CERTS",
    "GIT_SSL_CAINFO",
    "CARGO_HTTP_CAINFO",
    "PIP_CERT",
    "BUNDLE_SSL_CA_CERT",
    "npm_config_cafile",
    "NPM_CONFIG_CAFILE",
];
fn managed_ca_dir() -> Result<PathBuf> {
    let codex_home =
        find_codex_home().context("failed to resolve CODEX_HOME for managed MITM CA")?;
    Ok(codex_home.join(MANAGED_MITM_CA_DIR).to_path_buf())
}
fn is_generated_trust_bundle_path(path: &Path, proxy_dir: &Path) -> bool {
    is_generated_managed_ca_artifact_path(path, proxy_dir, MANAGED_MITM_CA_TRUST_BUNDLE_PREFIX)
}

fn is_generated_managed_ca_artifact_path(path: &Path, proxy_dir: &Path, prefix: &str) -> bool {
    let Some(file_name) = path.file_name().and_then(|file_name| file_name.to_str()) else {
        return false;
    };
    let Some(expected_hash) = file_name
        .strip_prefix(prefix)
        .and_then(|suffix| suffix.strip_prefix('-'))
        .and_then(|suffix| suffix.strip_suffix(".pem"))
    else {
        return false;
    };
    if path.parent() != Some(proxy_dir)
        || expected_hash.len() != 64
        || !expected_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return false;
    }
    let Ok(trust_bundle) = fs::read(path) else {
        return false;
    };
    format!("{:x}", Sha256::digest(trust_bundle)) == expected_hash
}

/// Returns whether `path` points at a current Codex-generated MITM CA bundle.
pub fn is_managed_mitm_ca_trust_bundle_path(path: &str) -> bool {
    let Ok(proxy_dir) = managed_ca_dir() else {
        return false;
    };
    let path = Path::new(path);
    is_generated_trust_bundle_path(path, &proxy_dir)
        || is_generated_trust_bundle_path(path, &proxy_dir.join(MANAGED_MITM_EXTERNAL_CA_DIR))
}
