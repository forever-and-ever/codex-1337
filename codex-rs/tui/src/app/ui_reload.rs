//! Reload at an event-loop boundary, never through the user-exit/shutdown path.
//!
//! Farm identities and saved submissions remain process-local. Refuse replacement
//! until they are resolved/discarded, preserving the v1 supervisor checkpoint format
//! and avoiding protocol-upgrade rollback loops in already-running supervisors.
//! Side threads survive the host handoff, but thread/resume requires stored history they lack.

use super::AGENTS_OVERVIEW_VIEW_ID;
use super::App;
use super::AppServerTarget;
use super::Duration;
use super::HashMap;
use super::PathBuf;
use super::Result;
use super::ThreadId;
use super::farm;

impl App {
    pub(super) async fn reload_ui(
        &mut self,
        idle_candidate: Option<PathBuf>,
    ) -> std::io::Result<()> {
        if self.local_shell.blocks_reload() {
            self.chat_widget.flash_local_shell_error(
                "Stop the local shell or restore its retained command before reloading the UI"
                    .to_owned(),
            );
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        #[cfg(target_os = "linux")]
        {
            let result =
                tokio::time::timeout(Duration::from_secs(/*secs*/ 2), self.capture_reload_input())
                    .await;
            let state = match result {
                Ok(Ok(state)) => state,
                Ok(Err(message)) => {
                    self.chat_widget.add_error_message(message);
                    return Err(std::io::ErrorKind::WouldBlock.into());
                }
                Err(_) => {
                    self.chat_widget.add_error_message(
                        "UI reload preparation timed out; nothing was replaced.".into(),
                    );
                    return Err(std::io::ErrorKind::WouldBlock.into());
                }
            };
            if let Err(error) = crate::reload::worker::reload(state, idle_candidate).await {
                self.chat_widget
                    .add_error_message(format!("UI reload failed: {error}"));
                return Err(error);
            }
        }
        #[cfg(not(target_os = "linux"))]
        self.chat_widget
            .add_error_message("UI reload is currently Linux-only.".into());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) fn can_reload_ui(&self) -> bool {
        !self.local_shell.blocks_reload()
            && crate::reload::worker::is_hosted()
            && !matches!(self.app_server_target, AppServerTarget::Embedded)
            && !self.reconnect.offline
            && self.chat_widget.can_reload_ui()
            && matches!(self.farm.state, farm::FarmRequestState::Idle)
            && self.queued_messages.mutations.is_empty()
            && self.server_operations.is_empty()
            && self.active_export.is_none()
            && self.pending_revert.is_none()
            && self.overlay.is_none()
            && self.jira.is_none()
            && self.temporary_structured_requests.is_empty()
            && self.pending_server_profiles.is_empty()
            && self.account_ui.selection_request.is_none()
            && self.pending_plugin_enabled_writes.is_empty()
            && self.pending_hook_enabled_writes.is_empty()
            && self.pending_shutdown_exit_thread_id.is_none()
            && !self.pending_startup_thread_start
            && self.agents_overview.new_session_draft.is_none()
            && self.agents_overview.parked_draft.is_none()
            && self.side_threads.is_empty()
    }

    #[cfg(target_os = "linux")]
    async fn capture_reload_input(&mut self) -> Result<crate::reload::RetainedInput, String> {
        if !self.side_threads.is_empty() {
            return Err(
                "Close the side conversation before reloading (ctrl+c in its empty composer)."
                    .into(),
            );
        }
        if self.agents_overview.parked_draft.is_some() {
            return Err("Use /new to return to your unsent draft before reloading.".into());
        }
        if !matches!(self.farm.state, farm::FarmRequestState::Idle) {
            return Err("Resolve/discard farm request before reload.".into());
        }
        if !self.can_reload_ui() {
            return Err(
                "Close the active dialog or let the pending UI action finish before reloading."
                    .into(),
            );
        }
        let primary = self.primary_thread_id;
        let selected = self.current_displayed_thread_id();
        let mut threads = HashMap::new();
        for (id, channel) in &self.thread_event_channels {
            let store = channel.store.lock().await;
            if let Some(input) = &store.input_state {
                threads.insert(*id, input.clone());
            }
        }
        let mut draft = self.chat_widget.capture_thread_input_state();
        if let Some(selected) = selected
            && let Some(input) = draft.take()
        {
            threads.insert(selected, input);
        }
        let unstarted = if self.chat_widget.session_draft.is_some() {
            draft
                .take()
                .map(|input| crate::reload::RetainedSessionDraft {
                    provider: self.chat_widget.config_ref().model_provider_id.clone(),
                    model: self.chat_widget.current_model().to_owned(),
                    cwd: self.chat_widget.config_ref().cwd.clone(),
                    input,
                })
        } else {
            None
        };
        let overview = self
            .chat_widget
            .selected_index_for_active_view(AGENTS_OVERVIEW_VIEW_ID)
            .is_some();
        let kill_buffer = self.chat_widget.take_kill_buffer_snapshot();
        // Preparation can fail without replacing the UI; retain its local copy too.
        self.chat_widget
            .restore_kill_buffer_snapshot(kill_buffer.clone());
        // A remote TUI's app-wide config cwd is a local placeholder, not the
        // task's directory. Never turn it into a remote --cd override on reload.
        let cwd = self
            .primary_session_configured
            .as_ref()
            .and_then(|session| session.cwd.to_abs_path().ok())
            .or_else(|| {
                (!self.app_server_target.uses_remote_workspace()).then(|| self.config.cwd.clone())
            });
        Ok(crate::reload::RetainedInput {
            primary,
            overview,
            draft,
            unstarted,
            kill_buffer,
            cwd,
            threads,
            terminal: crate::reload::worker::terminal_hints(),
            detached: self.agents_overview.input_states.clone(),
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn restore_reload_input(&mut self, thread: Option<ThreadId>) -> Result<()> {
        let Some(retained) = crate::reload::worker::take_input() else {
            return Ok(());
        };
        self.chat_widget
            .restore_kill_buffer_snapshot(retained.kill_buffer);
        self.agents_overview.input_states = retained.detached;
        if let Some(draft) = retained.unstarted {
            if retained.primary.is_some()
                || self.config.model_provider_id != draft.provider
                || self.config.cwd != draft.cwd
            {
                color_eyre::eyre::bail!(
                    "Cannot restore the unsent draft in a different provider or directory"
                );
            }
            self.chat_widget.restore_thread_input_state(
                Some(draft.input),
                crate::chatwidget::ThreadInputStateRestoreMode {
                    preserve_in_flight_turn: false,
                },
            );
            self.chat_widget.restore_session_draft();
        }
        if let Some(input) = retained.draft {
            self.chat_widget.restore_thread_input_state(
                Some(input),
                crate::chatwidget::ThreadInputStateRestoreMode {
                    preserve_in_flight_turn: true,
                },
            );
        }
        for (id, input) in retained.threads {
            if Some(id) == thread {
                let accepted_mode = self
                    .primary_session_configured
                    .as_ref()
                    .filter(|session| session.thread_id == id)
                    .filter(|session| session.collaboration_mode.is_some())
                    .map(|_| self.chat_widget.effective_collaboration_mode());
                self.chat_widget.restore_bound_thread_input_state(
                    Some(input),
                    crate::chatwidget::ThreadInputStateRestoreMode {
                        preserve_in_flight_turn: true,
                    },
                    crate::chatwidget::ThreadInputSettingsAuthority::Session { accepted_mode },
                );
                self.chat_widget
                    .set_queue_autosend_suppressed(/*suppressed*/ true);
            } else {
                self.ensure_thread_channel(id)
                    .store
                    .lock()
                    .await
                    .input_state = Some(input);
            }
        }
        Ok(())
    }
}
