use crate::{AuthManager, WorkspaceRoutingRequest};
use std::io;

pub(super) fn validate(owner: &AuthManager, request: &WorkspaceRoutingRequest) -> io::Result<()> {
    if !std::ptr::eq(owner, request.credentials.as_ref()) {
        return Err(io::Error::other(
            "workspace routing credential owner mismatch",
        ));
    }
    Ok(())
}
