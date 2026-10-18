use codex_api::AuthProvider;
use codex_config::read_openrouter_config;
use codex_protocol::KeyName;
use http::HeaderMap;
use http::HeaderValue;
use std::collections::HashSet;
use std::fmt;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use tokio::io::AsyncReadExt;

pub struct Bearer(HeaderValue);

impl fmt::Debug for Bearer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sk-or-…")
    }
}

impl Bearer {
    pub(super) fn expose(&self) -> HeaderValue {
        self.0.clone()
    }
}

impl AuthProvider for Bearer {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        let _ = headers.insert(http::header::AUTHORIZATION, self.expose());
    }
}

pub async fn credentials(codex_home: &Path, key: &KeyName) -> io::Result<Bearer> {
    let preferences = read_openrouter_config(codex_home)
        .await
        .map_err(|_| io::Error::other("Cannot read OpenRouter key preferences"))?;
    let path = preferences.key_files.get(key).ok_or_else(|| {
        io::Error::other(format!(
            "OpenRouter key {key} is unavailable; choose a key with /account"
        ))
    })?;
    read_key_file(path.as_path()).await
}

pub async fn read_key_file(path: &Path) -> io::Result<Bearer> {
    // All file errors are deliberately translated: OS diagnostics can contain
    // untrusted paths, while the secret must never reach an error or log.
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| io::Error::other("Cannot read the OpenRouter key file"))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|_| io::Error::other("Cannot inspect the OpenRouter key file"))?;
    let mut secret = String::new();
    let _ = file
        .take(16_385)
        .read_to_string(&mut secret)
        .await
        .map_err(|_| io::Error::other("Cannot read the OpenRouter key file"))?;
    if secret.len() > 16_384 {
        return Err(io::Error::other("OpenRouter key file is too large"));
    }
    let secret = secret.trim();
    if !secret.starts_with("sk-or-") || !secret.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(io::Error::other(
            "OpenRouter key file must contain a key starting with sk-or-",
        ));
    }
    if metadata.permissions().mode() & 0o044 != 0 {
        static WARNED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
        let first = WARNED
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.to_path_buf());
        if first {
            tracing::warn!(
                "An OpenRouter key file is group- or world-readable; restrict it to its owner"
            );
        }
    }
    let mut header = HeaderValue::from_str(&format!("Bearer {secret}"))
        .map_err(|_| io::Error::other("Invalid OpenRouter key file"))?;
    header.set_sensitive(true);
    Ok(Bearer(header))
}
