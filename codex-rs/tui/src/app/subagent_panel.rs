//! Integration hooks for the inline roster. Editing remains owned by the composer.
use super::app_server_event_targets::ServerNotificationThreadTarget;
use super::app_server_event_targets::server_notification_thread_target;
use super::*;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadStatus;

impl App {
    pub(super) fn sync_subagent_roster(&mut self) {
        // A replay channel can outlive the server thread. It does not make the
        // thread available for the inline switcher.
        {
            let roster = self.agent_navigation.roster.clone();
            let roster = roster
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for row in &roster.rows {
                if matches!(row.status, Some(ThreadStatus::NotLoaded)) {
                    self.agent_navigation.mark_closed(row.id);
                }
            }
        }
        let agents = self
            .agent_navigation
            .ordered_threads()
            .into_iter()
            .filter(|(id, entry)| {
                Some(*id) != self.primary_thread_id
                    && !self.side_threads.contains_key(id)
                    && !entry.is_closed
            })
            .map(|(id, entry)| {
                (
                    id,
                    entry
                        .agent_path
                        .clone()
                        .map(|path| path.trim_start_matches("/root/").to_string())
                        .or_else(|| entry.agent_nickname.clone())
                        .unwrap_or_else(|| id.to_string()),
                )
            })
            .collect();
        self.agent_navigation
            .roster
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reconcile(agents, self.current_displayed_thread_id());
        self.chat_widget.subagent_roster = self.agent_navigation.roster.clone();
    }

    pub(super) fn track_subagent_panel(&mut self, notification: &ServerNotification) {
        if let ServerNotification::ThreadDeleted(event) = notification
            && let Ok(id) = ThreadId::from_string(&event.thread_id)
            && self.agent_navigation.get(&id).is_some()
        {
            self.agent_navigation.remove(id);
            self.sync_subagent_roster();
            self.chat_widget.request_redraw();
            return;
        }
        if !matches!(
            notification,
            ServerNotification::ThreadStatusChanged(_)
                | ServerNotification::ThreadWorkStatusUpdated(_)
                | ServerNotification::ThreadTokenUsageUpdated(_)
                | ServerNotification::TurnStarted(_)
                | ServerNotification::TurnCompleted(_)
                | ServerNotification::ThreadClosed(_)
        ) {
            return;
        }
        let ServerNotificationThreadTarget::Thread(id) =
            server_notification_thread_target(notification)
        else {
            return;
        };
        // Authoritative liveness can restore a previously unloaded descendant.
        if let ServerNotification::ThreadStatusChanged(event) = notification
            && !matches!(event.status, ThreadStatus::NotLoaded)
            && let Some(entry) = self.agent_navigation.get(&id)
            && entry.is_closed
        {
            self.agent_navigation.upsert(
                id,
                entry.agent_nickname.clone(),
                entry.agent_role.clone(),
                /*is_closed*/ false,
            );
            self.sync_subagent_roster();
        }
        let mut roster = self
            .agent_navigation
            .roster
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(row) = roster.rows.iter_mut().find(|row| row.id == id) else {
            return;
        };
        match notification {
            ServerNotification::ThreadStatusChanged(event) => {
                if !matches!(event.status, ThreadStatus::Active { .. }) {
                    row.activity = None;
                }
                row.status = Some(event.status.clone());
            }
            ServerNotification::ThreadWorkStatusUpdated(event) => {
                if row
                    .status
                    .as_ref()
                    .is_none_or(|status| matches!(status, ThreadStatus::Active { .. }))
                {
                    row.activity = (!event.status.activity.is_empty()).then(|| {
                        event
                            .status
                            .activity
                            .chars()
                            .take(160)
                            .filter(|ch| !ch.is_control() || ch.is_whitespace())
                            .collect::<String>()
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    });
                }
            }
            ServerNotification::ThreadTokenUsageUpdated(event) => {
                row.tokens = Some(TokenUsage::from(&event.token_usage.total).blended_total())
            }
            ServerNotification::TurnStarted(_) => {
                row.status = Some(ThreadStatus::Active {
                    active_flags: Vec::new(),
                })
            }
            ServerNotification::TurnCompleted(event) => {
                row.activity = None;
                row.status = Some(if event.turn.error.is_some() {
                    ThreadStatus::SystemError
                } else {
                    ThreadStatus::Idle
                });
            }
            ServerNotification::ThreadClosed(_) => {
                row.status = Some(ThreadStatus::NotLoaded);
                row.activity = None;
            }
            _ => return,
        }
        roster.sort_by_activity();
        drop(roster);
        if matches!(notification, ServerNotification::ThreadClosed(_))
            || matches!(notification, ServerNotification::ThreadStatusChanged(event)
                if matches!(event.status, ThreadStatus::NotLoaded))
        {
            self.sync_subagent_roster();
        }
        self.chat_widget.request_redraw();
    }

    pub(super) async fn handle_subagent_panel_key(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        key: KeyEvent,
    ) -> bool {
        if self.overlay.is_some()
            || !self.chat_widget.no_modal_or_popup_active()
            || !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        {
            return false;
        }
        let watched_child = self.current_displayed_thread_id().filter(|id| {
            Some(*id) != self.primary_thread_id && !self.side_threads.contains_key(id)
        });
        let watch_only = watched_child.is_some() && self.chat_widget.is_parent_owned_thread();
        if watch_only && key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            if let Some(id) = watched_child
                && self.active_turn_id_for_thread(id).await.is_some()
                && let Err(error) = self
                    .submit_thread_op(server, id, AppCommand::Interrupt)
                    .await
            {
                self.chat_widget
                    .add_error_message(format!("Could not interrupt subagent: {error}"));
            }
            return true;
        }
        if !key.modifiers.is_empty() {
            return false;
        }
        if watch_only
            && self.chat_widget.composer_is_empty()
            && matches!(key.code, KeyCode::Char(c) if c != '/')
        {
            return true;
        }
        let target = {
            let mut roster = self
                .agent_navigation
                .roster
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if roster.focused.is_none() {
                if watch_only && key.code == KeyCode::Down {
                    roster.down();
                } else {
                    return false;
                }
                None
            } else {
                match key.code {
                    KeyCode::Down => {
                        roster.down();
                        None
                    }
                    KeyCode::Up => {
                        roster.up();
                        None
                    }
                    KeyCode::Esc => {
                        roster.focused = None;
                        None
                    }
                    KeyCode::Enter => {
                        let target = roster.focused;
                        roster.focused = None;
                        target
                    }
                    _ => return true,
                }
            }
        };
        if let Some(id) = target
            && let Err(error) = self.select_agent_thread(tui, server, id).await
        {
            self.chat_widget
                .add_error_message(format!("Could not open subagent: {error}"));
        }
        tui.frame_requester().schedule_frame();
        true
    }
}
