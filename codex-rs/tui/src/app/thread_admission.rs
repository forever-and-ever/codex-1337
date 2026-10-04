//! Conversation attachment is not proof of ancestry in the primary's spawn tree.

use super::*;
use codex_app_server_protocol::SessionSource;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadStatus;
use codex_protocol::protocol::SubAgentSource;

impl App {
    pub(super) fn is_family_thread(&self, id: ThreadId) -> bool {
        self.primary_thread_id.is_some_and(|root| {
            id == root || self.agent_navigation.descendant_roots.get(&id) == Some(&root)
        })
    }

    pub(super) fn is_attached_thread(&self, id: ThreadId) -> bool {
        self.is_family_thread(id)
            || self
                .primary_session_configured
                .as_ref()
                .is_some_and(|session| session.thread_id == id)
            || self.agent_navigation.get(&id).is_some()
            || self.side_threads.contains_key(&id)
            || self.current_displayed_thread_id() == Some(id)
    }

    pub(super) fn hydrate_started_thread(&mut self, thread: &Thread) {
        let Ok(id) = ThreadId::from_string(&thread.id) else {
            return;
        };
        let belongs_to_family = matches!(
            &thread.source,
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id, ..
            }) if self.is_family_thread(*parent_thread_id)
        );
        if !belongs_to_family && !self.is_attached_thread(id) {
            return;
        }
        self.upsert_agent_picker_thread(
            id,
            thread.agent_nickname.clone(),
            thread.agent_role.clone(),
            matches!(thread.status, ThreadStatus::NotLoaded),
        );
        // Retirement tombstones reject delayed starts as well as list snapshots.
        if self.agent_navigation.get(&id).is_none() {
            return;
        }
        if belongs_to_family && let Some(root) = self.primary_thread_id {
            self.agent_navigation.descendant_roots.insert(id, root);
        }
        self.agent_navigation.set_agent_path(
            id,
            crate::app_server_session::source_agent_path(&thread.source),
        );
        if thread.can_accept_direct_input == Some(false) {
            self.agent_navigation.mark_parent_owned(id);
        }
        let status = self
            .agent_navigation
            .status(id)
            .cloned()
            .unwrap_or_else(|| thread.status.clone());
        self.agent_navigation.note_status(id, status);
        self.agent_navigation.record_model(thread);
        self.sync_subagent_roster();
        self.refresh_visible_agent_picker();
        self.chat_widget.request_redraw();
    }
}
