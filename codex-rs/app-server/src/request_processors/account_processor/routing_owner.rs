use super::super::AccountRequestProcessor;
use codex_login::AuthManager;
use codex_login::WorkspaceRoutingRequest;
use std::sync::Arc;

// The retained owner prevents address reuse from aliasing another login.
#[derive(Clone)]
pub(super) struct CredentialOwner(pub(super) Arc<AuthManager>);

impl PartialEq for CredentialOwner {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_credential_authority(&other.0)
    }
}
impl Eq for CredentialOwner {}
impl std::hash::Hash for CredentialOwner {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Equal captured authorities always retain the same selected account.
        // Source/principal distinctions remain in equality, not wrapper addresses.
        std::hash::Hash::hash(&self.0.selected_account_id(), state);
    }
}

pub(super) fn scope(
    processor: &AccountRequestProcessor,
    request: &WorkspaceRoutingRequest,
) -> AccountRequestProcessor {
    let mut scoped = processor.clone();
    scoped.auth_manager = Arc::clone(&request.credentials);
    scoped
}

pub(super) fn capture_error(error: &std::io::Error) -> super::WorkspaceRoutingError {
    if let Some(failure) = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<codex_login::auth::RefreshTokenFailedError>())
    {
        return super::WorkspaceRoutingError::RefreshTokenRejected(failure.clone());
    }
    if error.kind() == std::io::ErrorKind::Interrupted {
        super::WorkspaceRoutingError::AccountChanged
    } else {
        super::WorkspaceRoutingError::DiscoveryFailed
    }
}
