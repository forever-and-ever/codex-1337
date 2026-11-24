use std::env;
use std::fmt;
use std::io;

/// A remote app-server credential with optional CLI environment provenance.
#[derive(Clone, PartialEq, Eq)]
pub struct RemoteAuthToken {
    secret: String,
    source_env: Option<String>,
}

impl RemoteAuthToken {
    /// Wraps an ad-hoc credential that cannot be used to relaunch a client.
    pub fn from_secret(secret: String) -> Self {
        Self {
            secret,
            source_env: None,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.secret
    }

    /// Returns the original environment name only while it supplies this credential.
    pub fn source_env_if_current(&self) -> Option<&str> {
        self.source_env_if_current_with(|name| env::var(name))
    }

    pub(crate) fn from_env(name: String) -> io::Result<Self> {
        Self::from_env_with(name, |name| env::var(name))
    }

    fn from_env_with(
        name: String,
        get_var: impl FnOnce(&str) -> Result<String, env::VarError>,
    ) -> io::Result<Self> {
        let secret = read_env(&name, get_var)?;
        Ok(Self {
            secret,
            source_env: Some(name),
        })
    }

    fn source_env_if_current_with(
        &self,
        get_var: impl FnOnce(&str) -> Result<String, env::VarError>,
    ) -> Option<&str> {
        let name = self.source_env.as_deref()?;
        (read_env(name, get_var).ok()?.as_str() == self.as_str()).then_some(name)
    }
}

impl fmt::Debug for RemoteAuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteAuthToken").finish_non_exhaustive()
    }
}

fn read_env(
    name: &str,
    get_var: impl FnOnce(&str) -> Result<String, env::VarError>,
) -> io::Result<String> {
    let value = get_var(name)
        .map_err(|_| io::Error::other(format!("environment variable `{name}` is not set")))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(io::Error::other(format!(
            "environment variable `{name}` is empty"
        )));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
#[path = "remote_auth_tests.rs"]
mod tests;
