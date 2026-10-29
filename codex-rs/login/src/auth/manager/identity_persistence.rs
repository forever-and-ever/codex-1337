//! Agent registration adds identity metadata without publishing a different login.

use super::AgentIdentityAuthRecord;
use super::AgentIdentityStorage;
use super::AuthDotJson;
use super::AuthStorageBackend;
use super::auth_storage_commit_guard;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;

pub(super) fn persist(
    auth_dot_json: &Arc<Mutex<Option<AuthDotJson>>>,
    storage: &Arc<dyn AuthStorageBackend>,
    record: AgentIdentityAuthRecord,
) -> io::Result<()> {
    let _commit = auth_storage_commit_guard()?;
    let mut guard = auth_dot_json
        .lock()
        .map_err(|_| io::Error::other("failed to lock auth state"))?;
    let previous = storage
        .load()?
        .ok_or_else(|| io::Error::other("auth data is not available"))?;
    if let Some(cached) = guard.as_ref() {
        let mut credentials = previous.clone();
        credentials
            .agent_identity
            .clone_from(&cached.agent_identity);
        // Older clients omit the login marker. An unchanged nonempty grant can
        // still identify that login; every other credential must remain exact.
        if let (Some(current), Some(expected)) =
            (credentials.tokens.as_mut(), cached.tokens.as_ref())
            && current.login_owner_id.is_none()
            && !current.refresh_token.is_empty()
            && current.refresh_token == expected.refresh_token
        {
            current.login_owner_id.clone_from(&expected.login_owner_id);
        }
        if &credentials != cached {
            return Err(io::Error::other(
                "Account changed during agent identity registration.",
            ));
        }
    }
    let mut auth = previous.clone();
    auth.agent_identity = Some(AgentIdentityStorage::Record(record));
    if !storage.compare_exchange(&previous, &auth)? {
        return Err(io::Error::other(
            "Account changed during agent identity registration.",
        ));
    }
    // Identity metadata never publishes another on-disk credential snapshot.
    if let Some(cached) = guard.as_mut() {
        cached.agent_identity = auth.agent_identity;
    }
    Ok(())
}
