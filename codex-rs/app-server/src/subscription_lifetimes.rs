use crate::outgoing_message::ConnectionId;
use codex_protocol::ThreadId;
use std::collections::HashSet;
use std::sync::Mutex;

/// Synchronous mirror of thread subscriptions, readable under the extension-lease lock without
/// awaiting. `ThreadStateManager` updates it wherever it adds or removes a subscription.
#[derive(Default)]
pub(crate) struct SubscriptionLifetimes(Mutex<HashSet<(ConnectionId, ThreadId)>>);

impl SubscriptionLifetimes {
    pub(crate) fn contains(&self, connection_id: ConnectionId, thread_id: ThreadId) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&(connection_id, thread_id))
    }

    pub(crate) fn insert(&self, connection_id: ConnectionId, thread_id: ThreadId) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((connection_id, thread_id));
    }

    pub(crate) fn remove(&self, connection_id: ConnectionId, thread_id: ThreadId) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(connection_id, thread_id));
    }

    pub(crate) fn remove_connection(&self, connection_id: ConnectionId) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(owner, _)| *owner != connection_id);
    }

    pub(crate) fn remove_thread(&self, thread_id: ThreadId) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(_, subscribed_thread)| *subscribed_thread != thread_id);
    }
}
