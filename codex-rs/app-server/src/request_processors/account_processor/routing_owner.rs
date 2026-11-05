use super::super::AccountRequestProcessor;
use codex_login::AuthManager;
use codex_login::WorkspaceRoutingRequest;
use std::sync::Arc;

// The retained owner prevents address reuse from aliasing another login.
#[derive(Clone)]
pub(super) struct CredentialOwner(pub(super) Arc<AuthManager>);

impl PartialEq for CredentialOwner {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for CredentialOwner {}
impl std::hash::Hash for CredentialOwner {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&Arc::as_ptr(&self.0), state);
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
