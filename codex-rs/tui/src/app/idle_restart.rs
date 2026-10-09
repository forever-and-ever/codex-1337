//! Restart only the foreground thin client; never run the ordinary shutdown path.

use super::*;

impl App {
    pub(super) fn ready_for_idle_restart(&self) -> bool {
        if !matches!(self.farm.state, farm::FarmRequestState::Idle)
            || !self.turn_submissions.is_empty()
            || self.thread_delivery.has_pending_work()
            || self.agents_overview.parked_draft.is_some()
        {
            return false;
        }
        #[cfg(target_os = "linux")]
        if crate::reload::worker::is_hosted() {
            return self.can_reload_ui();
        }
        matches!(self.app_server_target, AppServerTarget::LocalDaemon { .. })
            && !self.reconnect.offline
            && self.primary_thread_id.is_some()
            && self.current_displayed_thread_id() == self.primary_thread_id
            && self.overlay.is_none()
            && self.chat_widget.ready_for_idle_restart()
            && self.jira.is_none()
            && self.pending_primary_events.is_empty()
            && self.pending_app_server_requests.is_empty()
            && self.temporary_structured_requests.is_empty()
            && self.pending_server_profiles.is_empty()
            && self.pending_plugin_enabled_writes.is_empty()
            && self.pending_hook_enabled_writes.is_empty()
            && self.pending_shutdown_exit_thread_id.is_none()
            && !self.pending_startup_thread_start
            && !self.pending_managed_worktree_creation
    }

    pub(super) async fn try_idle_restart(
        &mut self,
        tui: &mut tui::Tui,
        restart: &mut crate::idle_restart::IdleRestart,
    ) {
        if !self.ready_for_idle_restart() {
            restart.postpone();
            return;
        }
        #[cfg(target_os = "linux")]
        if crate::reload::worker::is_hosted() {
            if let Some(candidate) = restart.replacement().await
                && let Err(error) = self.reload_ui(Some(candidate.clone())).await
            {
                restart.reject(candidate, &error);
            }
            return;
        }
        // A draft in another viewed thread is just as irreplaceable as the
        // foreground draft. Background server work itself may keep running.
        for (id, channel) in &self.thread_event_channels {
            let store = channel.store.lock().await;
            if store.side_parent_pending_status().is_some()
                || (Some(*id) != self.current_displayed_thread_id()
                    && store
                        .input_state
                        .as_ref()
                        .is_some_and(ThreadInputState::has_unsent_input))
            {
                restart.postpone();
                return;
            }
        }
        let Some(executable) = restart.replacement().await else {
            return;
        };
        let Some(thread) = self.primary_thread_id else {
            return;
        };
        let error = tui.with_restored(tui::TerminalHandoff::Restore, tui::RestoredInput::Preserve, || async {
            // The probe awaited a child process. Stopping the reader and checking
            // its queue closes the ordinary typeahead race before replacing it.
            match crossterm::event::poll(Duration::ZERO) {
                Ok(true) => std::io::Error::from(std::io::ErrorKind::WouldBlock),
                Ok(false) => {
                    tracing::info!(thread_id = %thread, executable = %executable.display(), "replacing idle TUI with installed build");
                    crate::idle_restart::reexec(&executable, thread, self.config.cwd.clone())
                }
                Err(error) => error,
            }
        }).await;
        restart.reject(executable, &error);
        self.chat_widget.request_redraw();
    }
}
