//! Report actual backend authority before a connection renders account capabilities.

use super::AccountNotification;
use super::AccountRequestProcessor;
use super::ConnectionId;

impl AccountRequestProcessor {
    pub(crate) async fn notify_initial_account_to_connection(&self, connection_id: ConnectionId) {
        let auth_changes = self.auth_manager.auth_change_state_receiver();
        let owner_generation = auth_changes.borrow().owner_generation;
        let notification = self.current_account_updated_notification();
        self.outgoing
            .send_account_notification(
                Some(connection_id),
                &auth_changes,
                owner_generation,
                AccountNotification::Updated(notification),
            )
            .await;
    }
}
