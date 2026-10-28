//! Permission admission belongs to the exact settings request, not its profile name.

use super::App;
use super::PermissionProfileSelection;
use super::ThreadId;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(in crate::app) struct PendingPermissionProfile {
    pub(in crate::app) request_id: Uuid,
    pub(in crate::app) selection: PermissionProfileSelection,
}

impl App {
    pub(in crate::app) fn owns_pending_permission_profile(
        &self,
        thread_id: ThreadId,
        request_id: Uuid,
    ) -> bool {
        self.pending_server_profiles
            .get(&thread_id)
            .is_some_and(|pending| pending.request_id == request_id)
    }
}
