//! Side-conversation thread lifecycle.
//!
//! A side conversation is a fork used for a quick /side question while keeping the
//! primary thread focused. This module owns the app-level lifecycle for those forks: switching into
//! them, returning to their parent, retaining them across normal navigation, and explicitly
//! discarding them. The fork receives hidden developer instructions that make inherited history
//! material only and steer the agent away from mutations unless the side conversation explicitly asks
//! for them.

use super::*;
use crate::app_event::SideStartMode;
use crate::app_server_session::SideChatStorage;
use crate::chatwidget::InterruptedTurnNoticeMode;

const SIDE_RENAME_BLOCK_MESSAGE: &str = "Side conversations cannot be renamed.";
const SIDE_MAIN_THREAD_UNAVAILABLE_MESSAGE: &str =
    "'/side' is unavailable until the main thread is ready.";
const SIDE_NO_STARTED_CONVERSATION_MESSAGE: &str = concat!(
    "'/side' is unavailable until the current conversation has started. ",
    "Send a message first, then try /side again."
);
pub(super) const SIDE_ALREADY_OPEN_MESSAGE: &str =
    "A side conversation is already open. Press ctrl + c to return before starting another.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SideParentStatus {
    NeedsInput,
    NeedsApproval,
    Failed,
    Interrupted,
    Closed,
    Finished,
}

impl SideParentStatus {
    fn label(self, parent_is_main: bool) -> &'static str {
        match (self, parent_is_main) {
            (SideParentStatus::NeedsInput, true) => "main needs input",
            (SideParentStatus::NeedsInput, false) => "parent needs input",
            (SideParentStatus::NeedsApproval, true) => "main needs approval",
            (SideParentStatus::NeedsApproval, false) => "parent needs approval",
            (SideParentStatus::Failed, true) => "main failed",
            (SideParentStatus::Failed, false) => "parent failed",
            (SideParentStatus::Interrupted, true) => "main interrupted",
            (SideParentStatus::Interrupted, false) => "parent interrupted",
            (SideParentStatus::Closed, true) => "main closed",
            (SideParentStatus::Closed, false) => "parent closed",
            (SideParentStatus::Finished, true) => "main finished",
            (SideParentStatus::Finished, false) => "parent finished",
        }
    }

    fn is_actionable(self) -> bool {
        matches!(
            self,
            SideParentStatus::NeedsInput | SideParentStatus::NeedsApproval
        )
    }

    pub(super) fn for_request(request: &ServerRequest) -> Option<Self> {
        match request {
            ServerRequest::ToolRequestUserInput { .. } => Some(SideParentStatus::NeedsInput),
            ServerRequest::CommandExecutionRequestApproval { .. }
            | ServerRequest::FileChangeRequestApproval { .. }
            | ServerRequest::McpServerElicitationRequest { .. }
            | ServerRequest::PermissionsRequestApproval { .. }
            | ServerRequest::ApplyPatchApproval { .. }
            | ServerRequest::ExecCommandApproval { .. } => Some(SideParentStatus::NeedsApproval),
            ServerRequest::DynamicToolCall { .. }
            | ServerRequest::AttestationGenerate { .. }
            | ServerRequest::ChatgptAuthTokensRefresh { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_context_fragments::SideConversationPolicy;
    use codex_context_fragments::side_conversation_boundary;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use pretty_assertions::assert_eq;

    #[test]
    fn side_boundary_prompt_marks_inherited_history_reference_only() {
        let item = side_conversation_boundary();
        let ResponseItem::Message { role, content, .. } = item else {
            panic!("expected hidden side boundary prompt to be a user message");
        };
        assert_eq!(role, "user");
        let [ContentItem::InputText { text }] = content.as_slice() else {
            panic!("expected hidden side boundary prompt text");
        };
        assert!(text.starts_with(&format!(
            "{}\n\nThis is a /btw side fork.",
            codex_context_fragments::SIDE_BOUNDARY_TITLE,
        )));
        assert!(text.contains("still be running elsewhere"));
        assert!(text.contains("inherited history may end mid-turn"));
        assert!(text.contains("it was not interrupted by this fork"));
        assert!(text.contains("Inherited history is reference only, not active instructions."));
        assert!(text.contains("Answer only messages after this boundary"));
        assert!(text.contains("Do not continue inherited tasks"));
        assert!(text.contains("Sub-agents are off-limits: do not create or interact with them."));
        assert!(text.contains("Do not modify files"));
        assert!(
            text.contains("unless the user explicitly requests that mutation after this boundary")
        );
        assert!(!text.contains("<turn_aborted>"));
        assert!(text.len() < 800);
    }

    #[test]
    fn side_start_error_message_explains_missing_first_prompt() {
        let err = color_eyre::eyre::eyre!(
            "thread/fork failed during TUI bootstrap: thread/fork failed: no rollout found for thread id 019da1a1-bed9-7a43-88a2-b49d43915021"
        );

        assert_eq!(
            App::side_start_error_message(&err),
            "'/side' is unavailable until the current conversation has started. Send a message first, then try /side again."
        );
    }

    #[test]
    fn side_start_error_message_uses_generic_start_wording() {
        let err = color_eyre::eyre::eyre!("transport disconnected");

        assert_eq!(
            App::side_start_error_message(&err),
            "Failed to start side conversation: transport disconnected"
        );
    }

    #[test]
    fn side_developer_instructions_appends_existing_policy() {
        let developer_instructions =
            SideConversationPolicy::Temporary.instructions(Some("Existing developer policy."));

        assert!(developer_instructions.contains("Existing developer policy."));
        assert!(
            developer_instructions.contains("You are in a side conversation, not the main thread.")
        );
        assert!(
            developer_instructions.contains("Sub-agents are off-limits in this side conversation.")
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SideParentStatusChange {
    Set(SideParentStatus),
    Clear,
    ClearActionable,
}

impl SideParentStatusChange {
    pub(super) fn for_notification(notification: &ServerNotification) -> Option<Self> {
        match notification {
            ServerNotification::TurnStarted(_) => Some(SideParentStatusChange::Clear),
            ServerNotification::TurnCompleted(notification) => match &notification.turn.status {
                TurnStatus::Completed => {
                    Some(SideParentStatusChange::Set(SideParentStatus::Finished))
                }
                TurnStatus::Interrupted => {
                    Some(SideParentStatusChange::Set(SideParentStatus::Interrupted))
                }
                TurnStatus::Failed => Some(SideParentStatusChange::Set(SideParentStatus::Failed)),
                TurnStatus::InProgress => None,
            },
            ServerNotification::ThreadClosed(_) => {
                Some(SideParentStatusChange::Set(SideParentStatus::Closed))
            }
            ServerNotification::ItemStarted(_) | ServerNotification::ServerRequestResolved(_) => {
                Some(SideParentStatusChange::ClearActionable)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct SideThreadState {
    /// Thread to return to when the current side conversation is dismissed.
    pub(super) parent_thread_id: ThreadId,
    /// Parent-thread condition that changed while this side thread is visible.
    pub(super) parent_status: Option<SideParentStatus>,
    pub(super) opening_message: Option<crate::chatwidget::UserMessage>,
    pub(super) close_request: Option<uuid::Uuid>,
    pub(super) storage: SideChatStorage,
}

impl SideThreadState {
    pub(super) fn new(parent_thread_id: ThreadId) -> Self {
        Self {
            parent_thread_id,
            parent_status: None,
            opening_message: None,
            close_request: None,
            storage: SideChatStorage::Temporary,
        }
    }
}

impl App {
    pub(super) async fn running_side_thread_id(&self) -> Option<ThreadId> {
        for thread_id in self.side_threads.keys().copied() {
            if self.active_turn_id_for_thread(thread_id).await.is_some() {
                return Some(thread_id);
            }
        }
        None
    }

    pub(super) fn sync_side_thread_ui(&mut self) {
        if self.zoomed_thread.is_some() && self.zoomed_thread != self.open_pane_focus() {
            self.zoomed_thread = None;
        }
        let clear_side_ui = |chat_widget: &mut crate::chatwidget::ChatWidget| {
            chat_widget.set_side_conversation_context_label(/*label*/ None);
            chat_widget.set_side_conversation_active(/*active*/ false);
            chat_widget.clear_thread_rename_block();
            chat_widget.set_interrupted_turn_notice_mode(InterruptedTurnNoticeMode::Default);
        };
        let Some(active_thread_id) = self.current_displayed_thread_id() else {
            clear_side_ui(&mut self.chat_widget);
            return;
        };
        let Some((parent_thread_id, parent_status, storage)) = self
            .side_threads
            .get(&active_thread_id)
            .map(|state| (state.parent_thread_id, state.parent_status, state.storage))
        else {
            clear_side_ui(&mut self.chat_widget);
            if self
                .side_threads
                .values()
                .any(|state| state.parent_thread_id == active_thread_id)
                && let Some(binding) = self.keymap.primary_hint(
                    crate::keymap::KeymapContext::Global,
                    "toggle_side_conversation",
                )
            {
                self.chat_widget
                    .set_side_conversation_context_label(Some(format!(
                        "{} for side",
                        binding.display_label()
                    )));
            }
            return;
        };

        match storage {
            SideChatStorage::Attached => self.chat_widget.clear_thread_rename_block(),
            SideChatStorage::Temporary => self
                .chat_widget
                .set_thread_rename_block_message(SIDE_RENAME_BLOCK_MESSAGE),
        }
        self.chat_widget
            .set_side_conversation_active(/*active*/ true);
        self.chat_widget
            .set_interrupted_turn_notice_mode(InterruptedTurnNoticeMode::Suppress);
        let mut label_parts = Vec::new();
        let parent_is_main = self.primary_thread_id == Some(parent_thread_id);
        if parent_is_main {
            label_parts.push("from main thread".to_string());
        } else {
            let parent_label = self.thread_label(parent_thread_id);
            label_parts.push(format!("from parent thread ({parent_label})"));
        }
        if let Some(parent_status) = parent_status {
            label_parts.push(parent_status.label(parent_is_main).to_string());
        }
        if let Some(binding) = self.keymap.primary_hint(
            crate::keymap::KeymapContext::Global,
            "toggle_side_conversation",
        ) {
            label_parts.push(format!("{} to switch", binding.display_label()));
        }
        let action = if storage == SideChatStorage::Attached
            && self.local_settings.workspace_enabled(&self.config)
        {
            "archive"
        } else {
            "close"
        };
        label_parts.push(format!(
            "{} to {action}",
            crate::key_hint::ctrl(KeyCode::Char('c')).display_label()
        ));
        self.chat_widget
            .set_side_conversation_context_label(Some(format!("Side {}", label_parts.join(" · "))));
    }

    pub(super) fn active_side_parent_thread_id(&self) -> Option<ThreadId> {
        self.current_displayed_thread_id()
            .and_then(|thread_id| self.side_threads.get(&thread_id))
            .map(|state| state.parent_thread_id)
    }

    pub(super) fn side_retention_enabled(&self, tui: &tui::Tui) -> bool {
        self.local_settings.workspace_enabled(&self.config) && tui.is_owned_screen()
    }

    pub(super) fn conversation_focus_available(&self, tui: &tui::Tui) -> bool {
        self.side_retention_enabled(tui)
            && self.overlay.is_none()
            && (self.chat_widget.no_modal_or_popup_active()
                // Replaying the parent may reopen a completion for its parked draft.
                // The focused chooser hides it and owns the keystrokes.
                || (self.chooser_focus() == Some(sidebar::PaneFocus::Sidebar)
                    && self.sidebar_chooser_available()
                    && self.chat_widget.completion_can_yield_focus()))
    }

    pub(super) fn main_composer_shortcut_available(&self) -> bool {
        !self.reconnect.offline || self.active_side_parent_thread_id().is_none()
    }

    /// Zoom follows the displayed conversation even when its peer cannot be selected offline.
    pub(super) fn open_pane_focus(&self) -> Option<ThreadId> {
        let focused = self.current_displayed_thread_id()?;
        let child = self.side_pair_key(focused)?;
        (self.sidebar()?.visible_content() == Some(sidebar::SidebarContent::Chat(child)))
            .then_some(focused)
    }

    pub(super) fn switchable_side_child(&self) -> Option<ThreadId> {
        if self.reconnect.offline {
            return None;
        }
        self.side_pair_key(self.current_displayed_thread_id()?)
    }

    pub(super) fn side_pair_key(&self, thread_id: ThreadId) -> Option<ThreadId> {
        if self.side_threads.contains_key(&thread_id) {
            Some(thread_id)
        } else {
            self.side_thread_for_parent(thread_id)
        }
    }

    pub(super) fn side_thread_for_parent(&self, parent_thread_id: ThreadId) -> Option<ThreadId> {
        self.sidebars
            .get(&Some(parent_thread_id))
            .and_then(sidebar::SidebarPane::selected_chat)
            .or_else(|| {
                self.side_threads.iter().find_map(|(thread_id, state)| {
                    (state.parent_thread_id == parent_thread_id).then_some(*thread_id)
                })
            })
    }

    pub(super) fn set_side_parent_status(
        &mut self,
        parent_thread_id: ThreadId,
        status: Option<SideParentStatus>,
    ) {
        let mut changed = false;
        for state in self
            .side_threads
            .values_mut()
            .filter(|state| state.parent_thread_id == parent_thread_id)
        {
            if state.parent_status != status {
                state.parent_status = status;
                changed = true;
            }
        }
        if changed {
            self.sync_side_thread_ui();
        }
    }

    pub(super) fn clear_side_parent_action_status(&mut self, parent_thread_id: ThreadId) {
        let mut changed = false;
        for state in self
            .side_threads
            .values_mut()
            .filter(|state| state.parent_thread_id == parent_thread_id)
        {
            if state
                .parent_status
                .is_some_and(SideParentStatus::is_actionable)
            {
                state.parent_status = None;
                changed = true;
            }
        }
        if changed {
            self.sync_side_thread_ui();
        }
    }

    pub(super) fn apply_side_parent_status_change(
        &mut self,
        parent_thread_id: ThreadId,
        change: SideParentStatusChange,
    ) {
        match change {
            SideParentStatusChange::Set(status) => {
                self.set_side_parent_status(parent_thread_id, Some(status));
            }
            SideParentStatusChange::Clear => {
                self.set_side_parent_status(parent_thread_id, /*status*/ None);
            }
            SideParentStatusChange::ClearActionable => {
                self.clear_side_parent_action_status(parent_thread_id);
            }
        }
    }

    pub(super) async fn maybe_return_from_side(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        key: KeyEvent,
    ) -> bool {
        if self.overlay.is_none()
            && self.chat_widget.no_modal_or_popup_active()
            && self.chat_widget.composer_is_empty()
            && let Some(target) = self.current_displayed_thread_id()
            && let Some(side) = self.side_threads.get(&target)
        {
            if matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&'c'))
                && self.side_retention_enabled(tui)
                && self.sidebar_chooser_available()
                && !self.transcript_view.owns_interaction_key(key)
                && side.storage == SideChatStorage::Attached
            {
                if let Err(err) = sidebar_close::request(
                    self,
                    tui,
                    app_server,
                    target,
                    sidebar_close::Action::Archive,
                )
                .await
                {
                    self.chat_widget.add_error_message(err.to_string());
                }
                return true;
            }
            if self
                .select_agent_thread_and_discard_side(tui, app_server, side.parent_thread_id)
                .await
                .is_err()
            {
                return false;
            }
            true
        } else {
            false
        }
    }

    pub(super) fn side_thread_to_discard_after_switch(
        &self,
        target_thread_id: ThreadId,
    ) -> Option<ThreadId> {
        let active_thread_id = self.current_displayed_thread_id()?;
        let state = self.side_threads.get(&active_thread_id)?;
        (target_thread_id == state.parent_thread_id).then_some(active_thread_id)
    }

    pub(super) async fn toggle_side_conversation(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
    ) -> Result<()> {
        let Some(active_thread_id) = self.current_displayed_thread_id() else {
            return Ok(());
        };
        let target_thread_id = if let Some(state) = self.side_threads.get(&active_thread_id) {
            state.parent_thread_id
        } else if let Some(side_thread_id) = self.side_thread_for_parent(active_thread_id) {
            side_thread_id
        } else {
            return Ok(());
        };

        self.select_agent_thread_after(
            tui,
            app_server,
            target_thread_id,
            super::turn_submissions::SelectionAfter::SurfaceInactive,
        )
        .await?;
        Ok(())
    }

    pub(super) async fn hide_focused_side_pane(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
    ) -> Result<()> {
        let Some(child) = self.current_displayed_thread_id() else {
            return Ok(());
        };
        let Some(side) = self.side_threads.get(&child) else {
            return Ok(());
        };
        let parent = side.parent_thread_id;
        self.select_agent_thread_after(
            tui,
            app_server,
            parent,
            super::turn_submissions::SelectionAfter::HideSide { parent },
        )
        .await?;
        Ok(())
    }

    pub(super) async fn discard_side_thread(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        self.pending_app_server_requests
            .cancel_thread_verification(&thread_id.to_string());
        self.enqueue_side_cleanup(app_server, thread_id).await;
    }

    pub(super) async fn discard_side_thread_in_background(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        self.discard_side_thread(app_server, thread_id).await;
    }

    pub(super) async fn discard_closed_side_thread(&mut self, thread_id: ThreadId) {
        self.discard_thread_local_state(thread_id).await;
    }

    pub(super) async fn discard_thread_local_state(&mut self, thread_id: ThreadId) {
        self.pending_app_server_requests
            .cancel_thread_verification(&thread_id.to_string());
        self.abort_thread_event_listener(thread_id);
        self.thread_event_channels.remove(&thread_id);
        self.pending_server_profiles.remove(&thread_id);
        self.agents_overview.activity.remove(&thread_id);
        self.sidebars.remove(&Some(thread_id));
        if let Some(side) = self.side_threads.remove(&thread_id)
            && let Some(pane) = self.sidebars.get_mut(&Some(side.parent_thread_id))
        {
            pane.remove_chat(thread_id, side.storage);
        }
        self.side_companions.remove(&thread_id);
        self.agent_navigation.remove(thread_id);
        if self.active_thread_id == Some(thread_id) {
            self.clear_active_thread().await;
        } else {
            self.refresh_pending_thread_approvals().await;
        }
        self.forget_realtime_replay_thread(thread_id);
        self.sync_active_agent_label();
    }

    pub(super) fn side_fork_config(&self) -> Config {
        let mut fork_config = self.chat_widget.config_ref().clone();
        let parent_model = self.chat_widget.current_model();
        if !parent_model.trim().is_empty() {
            fork_config.model = Some(parent_model.to_string());
        }
        fork_config.model_reasoning_effort = self.chat_widget.current_reasoning_effort();
        fork_config.service_tier = self.chat_widget.configured_service_tier();
        fork_config.daybreak_enabled = false;
        fork_config
    }

    pub(super) fn side_start_block_message(&self) -> Option<&'static str> {
        if self.primary_thread_id.is_none() {
            Some(SIDE_MAIN_THREAD_UNAVAILABLE_MESSAGE)
        } else if self.active_side_parent_thread_id().is_some() {
            Some(SIDE_ALREADY_OPEN_MESSAGE)
        } else {
            None
        }
    }

    pub(super) fn side_start_error_message(err: &color_eyre::Report) -> String {
        if err.chain().any(|cause| {
            let message = cause.to_string();
            message.contains("no rollout found for thread id")
                || message.contains("includeTurns is unavailable before first user message")
        }) {
            SIDE_NO_STARTED_CONVERSATION_MESSAGE.to_string()
        } else {
            format!("Failed to start side conversation: {err}")
        }
    }

    pub(super) fn fail_side_creation(
        &mut self,
        parent: ThreadId,
        message: String,
        prompt: Option<crate::chatwidget::UserMessage>,
        cleanup: Option<ThreadId>,
    ) {
        if let Some(pane) = self.sidebars.get_mut(&Some(parent))
            && let Some(creation) = &mut pane.creation
        {
            creation.state = sidebar::CreationState::Failed { message, cleanup };
            creation.message = prompt;
        } else {
            self.restore_side_user_message(prompt);
            self.chat_widget.add_error_message(message);
        }
    }

    pub(super) fn restore_side_user_message(
        &mut self,
        user_message: Option<crate::chatwidget::UserMessage>,
    ) {
        if let Some(user_message) = user_message {
            self.chat_widget
                .restore_user_message_to_composer(user_message);
        }
    }

    pub(super) fn install_side_thread_snapshot(
        store: &mut ThreadEventStore,
        mut session: ThreadSessionState,
        _forked_turns: Vec<Turn>,
    ) {
        // The forked history remains available to the model through core state, but side
        // conversations should visually start at the side boundary.
        session.forked_from_id = None;
        store.set_session(session, Vec::new());
    }

    pub(super) async fn select_agent_thread_and_discard_side(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> Result<()> {
        let after = self
            .side_thread_to_discard_after_switch(thread_id)
            .map(super::turn_submissions::SelectionAfter::DiscardSide)
            .unwrap_or(super::turn_submissions::SelectionAfter::SelectOnly);
        self.select_agent_thread_after(tui, app_server, thread_id, after)
            .await
    }

    pub(super) async fn handle_start_side(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        parent_thread_id: ThreadId,
        mut user_message: Option<crate::chatwidget::UserMessage>,
        mode: SideStartMode,
    ) -> Result<AppRunControl> {
        let workspace = self.side_retention_enabled(tui);
        let existing = (mode == SideStartMode::Reuse || !workspace)
            .then(|| self.side_thread_for_parent(parent_thread_id))
            .flatten();
        if let Some(pane) = self.sidebars.get_mut(&Some(parent_thread_id))
            && pane.creation.is_some()
        {
            // Retry and Back consume the saved attempt. A new /side must not overwrite its prompt.
            pane.show(
                sidebar::SidebarContent::Chooser,
                sidebar::PaneFocus::Sidebar,
            );
            tui.frame_requester().schedule_frame();
            self.restore_side_user_message(user_message);
            return Ok(AppRunControl::Continue);
        }

        if let Some(message) = self.side_start_block_message() {
            self.restore_side_user_message(user_message.take());
            self.sync_side_thread_ui();
            self.chat_widget.add_error_message(message.to_string());
            return Ok(AppRunControl::Continue);
        }
        if self.pending_server_profiles.contains_key(&parent_thread_id) {
            self.restore_side_user_message(user_message.take());
            self.sync_side_thread_ui();
            self.chat_widget
                .add_error_message("Wait for permissions to update before forking.".into());
            return Ok(AppRunControl::Continue);
        }

        if let Some(side_thread_id) = existing {
            self.select_tracked_side_locally(
                tui,
                app_server,
                side_thread_id,
                super::turn_submissions::SelectionAfter::SelectOnly,
            )
            .await?;
            if self.active_thread_id == Some(side_thread_id) {
                if let Some(message) = user_message.take()
                    && let Err(message) = self
                        .chat_widget
                        .submit_plain_user_turn_to(side_thread_id, message)
                {
                    self.restore_side_user_message(Some(message));
                }
            } else {
                self.restore_side_user_message(user_message.take());
            }
            return Ok(AppRunControl::Continue);
        }

        Ok(self.start_side_fork(app_server, parent_thread_id, user_message, workspace))
    }
}
