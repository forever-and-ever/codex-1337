//! One selected backend owns its request handle and cancellation-safe event stream.

use crate::app_server_session::AppServerSession;
use crate::app_server_session::AppServerSessionEvent;
use crate::backend_id::BackendId;
use std::collections::VecDeque;

enum StreamState {
    Listening,
    Closed,
}

pub(super) struct BackendSession {
    session: AppServerSession,
    stream: StreamState,
    deferred_startup: VecDeque<AppServerSessionEvent>,
}

impl BackendSession {
    pub(super) fn new(session: AppServerSession) -> Self {
        Self {
            session,
            stream: StreamState::Listening,
            deferred_startup: VecDeque::new(),
        }
    }

    pub(super) fn owner(&self) -> BackendId {
        self.session.backend_id()
    }

    pub(super) fn session(&self) -> &AppServerSession {
        &self.session
    }

    pub(super) fn session_mut(&mut self) -> &mut AppServerSession {
        &mut self.session
    }

    pub(super) fn is_listening(&self, owner: BackendId) -> bool {
        owner == self.owner() && matches!(self.stream, StreamState::Listening)
    }

    pub(super) fn resume_events(&mut self, owner: BackendId) {
        assert_eq!(
            owner,
            self.owner(),
            "reconnect belongs to the selected backend"
        );
        // A reconnect installs a new source epoch. Its fresh snapshot supersedes the
        // old connection's unregistered-child notifications; never replay them into new custody.
        self.deferred_startup.clear();
        self.stream = StreamState::Listening;
    }

    /// Keep pre-registration child notifications with their originating connection.
    pub(super) fn defer_startup(&mut self, event: AppServerSessionEvent) {
        if event.backend_id == self.owner() {
            self.deferred_startup.push_back(event);
        } else {
            tracing::warn!("ignoring deferred startup notification from a different backend");
        }
    }

    pub(super) fn take_ready_startup(
        &mut self,
        ready: impl FnMut(&AppServerSessionEvent) -> bool,
    ) -> Option<AppServerSessionEvent> {
        let index = self.deferred_startup.iter().position(ready)?;
        self.deferred_startup.remove(index)
    }

    pub(super) async fn next_event(
        &mut self,
        paused: bool,
    ) -> (BackendId, Option<AppServerSessionEvent>) {
        if paused || matches!(self.stream, StreamState::Closed) {
            return std::future::pending().await;
        }
        let owner = self.owner();
        // Dropping a pending receive leaves the queued event available to the next poll.
        let event = self.session.next_event().await;
        if event.is_none() {
            self.stream = StreamState::Closed;
        }
        (owner, event)
    }

    pub(super) async fn shutdown(self) -> std::io::Result<()> {
        self.session.shutdown().await
    }
}
