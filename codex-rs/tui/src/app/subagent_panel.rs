//! Integration hooks for the inline roster. Typing yields focus to the composer.
use super::app_server_event_targets::ServerNotificationThreadTarget;
use super::app_server_event_targets::server_notification_thread_target;
use super::*;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadStatus;

impl App {
    pub(super) fn sync_subagent_roster(&mut self) {
        let agents = self
            .agent_navigation
            .ordered_threads()
            .into_iter()
            .filter(|(id, entry)| {
                !matches!(
                    self.agent_navigation.status(*id),
                    Some(ThreadStatus::NotLoaded)
                ) && !entry.is_closed
                    && Some(*id) != self.primary_thread_id
                    && !self.side_threads.contains_key(id)
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
        let mut roster = self
            .agent_navigation
            .roster
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        roster.reconcile(agents, self.current_displayed_thread_id());
        for row in &mut roster.rows {
            row.model = self.agent_navigation.models.get(&row.id).cloned();
            row.status = self.agent_navigation.status(row.id).cloned();
            if row
                .status
                .as_ref()
                .is_some_and(|status| !matches!(status, ThreadStatus::Active { .. }))
            {
                row.activity = None;
            }
        }
        roster.sort_by_activity();
        drop(roster);
        self.chat_widget.subagent_roster = self.agent_navigation.roster.clone();
    }

    pub(super) fn track_subagent_panel(&mut self, notification: &ServerNotification) {
        match notification {
            ServerNotification::ThreadStarted(event) => {
                self.hydrate_started_thread(&event.thread);
                return;
            }
            ServerNotification::ThreadSettingsUpdated(event) => {
                if let Ok(id) = ThreadId::from_string(&event.thread_id)
                    && self.agent_navigation.get(&id).is_some()
                {
                    self.agent_navigation.models.insert(
                        id,
                        crate::subagent_roster::AgentModel {
                            model: event.thread_settings.model.clone(),
                            effort: event.thread_settings.effort.clone(),
                        },
                    );
                    self.sync_subagent_roster();
                    self.chat_widget.request_redraw();
                }
                return;
            }
            _ => {}
        }
        if let ServerNotification::ThreadDeleted(event) = notification
            && let Ok(id) = ThreadId::from_string(&event.thread_id)
            && self.agent_navigation.get(&id).is_some()
        {
            self.agent_navigation.remove(id);
            self.sync_subagent_roster();
            self.refresh_visible_agent_picker();
            self.chat_widget.request_redraw();
            return;
        }
        if !matches!(
            notification,
            ServerNotification::ThreadStatusChanged(_)
                | ServerNotification::ThreadWorkStatusUpdated(_)
                | ServerNotification::ThreadTokenUsageUpdated(_)
                | ServerNotification::ThreadClosed(_)
        ) {
            return;
        }
        let ServerNotificationThreadTarget::Thread(id) =
            server_notification_thread_target(notification)
        else {
            return;
        };
        match notification {
            ServerNotification::ThreadStatusChanged(event) => {
                self.agent_navigation.note_status(id, event.status.clone());
            }
            ServerNotification::ThreadClosed(_) => {
                self.agent_navigation
                    .note_status(id, ThreadStatus::NotLoaded);
            }
            _ => {}
        }
        if self.agent_navigation.get(&id).is_none() && Some(id) != self.primary_thread_id {
            return;
        }
        if matches!(
            notification,
            ServerNotification::ThreadStatusChanged(_) | ServerNotification::ThreadClosed(_)
        ) {
            self.sync_subagent_roster();
            self.refresh_visible_agent_picker();
            self.chat_widget.request_redraw();
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
            _ => return,
        }
        roster.sort_by_activity();
        drop(roster);
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
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('x') {
            let target = self
                .agent_navigation
                .roster
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .focused
                .or(watched_child);
            if let Some(target) = target {
                self.begin_agent_retirement(server, target);
                tui.frame_requester().schedule_frame();
                return true;
            }
        }
        let retirement_blocks_navigation = {
            let roster = self
                .agent_navigation
                .roster
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            roster.retiring.is_some() && (roster.focused.is_some() || watch_only)
        };
        if retirement_blocks_navigation && key.code != KeyCode::Esc {
            return true;
        }
        if matches!(key.code, KeyCode::Char(c) if !c.is_control())
            && (key.modifiers - KeyModifiers::SHIFT).is_empty()
        {
            let mut roster = self
                .agent_navigation
                .roster
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            roster.focused = None;
            roster.sort_by_activity();
            return false;
        }
        if !key.modifiers.is_empty() {
            return false;
        }
        let target = {
            let mut roster = self
                .agent_navigation
                .roster
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if roster.focused.is_none() {
                if watch_only && self.chat_widget.composer_is_empty() && key.code == KeyCode::Down {
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
                        roster.sort_by_activity();
                        None
                    }
                    KeyCode::Enter => {
                        let target = roster.focused;
                        roster.focused = None;
                        roster.sort_by_activity();
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

    fn begin_agent_retirement(&mut self, server: &AppServerSession, target: ThreadId) {
        let Some(root) = self.primary_thread_id else {
            return;
        };
        if root == target {
            return;
        }
        let mut roster = self
            .agent_navigation
            .roster
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if roster.retiring.is_some() {
            return;
        }
        roster.retiring = Some(crate::subagent_roster::Retirement::Pending);
        drop(roster);
        let request = server.request_handle();
        let events = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = request
                .request_typed::<codex_app_server_protocol::ThreadRetireResponse>(
                    ClientRequest::ThreadRetire {
                        request_id: codex_app_server_protocol::RequestId::String(
                            Uuid::new_v4().to_string(),
                        ),
                        params: codex_app_server_protocol::ThreadRetireParams {
                            root_thread_id: root.to_string(),
                            thread_id: target.to_string(),
                        },
                    },
                )
                .await
                .map(|response| response.thread_ids)
                .map_err(|error| error.to_string());
            events.send(AppEvent::AgentRetired {
                root,
                target,
                result,
            });
        });
    }

    pub(super) async fn finish_agent_retirement(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        root: ThreadId,
        target: ThreadId,
        result: Result<Vec<String>, String>,
    ) -> Result<()> {
        if self.primary_thread_id != Some(root) {
            return Ok(());
        }
        match result {
            Ok(ids) => {
                self.agent_navigation
                    .roster
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retiring = None;
                let ids = ids
                    .iter()
                    .filter_map(|id| ThreadId::from_string(id).ok())
                    .collect::<Vec<_>>();
                self.forget_retired_agents(&ids).await;
            }
            Err(error) => {
                self.chat_widget
                    .add_error_message(format!("Could not confirm retirement: {error}"));
                self.agent_navigation
                    .roster
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retiring = Some(crate::subagent_roster::Retirement::Reconciling(target));
                // A transport failure can follow a committed retirement. Only a fresh,
                // authoritative roster may release the operation lease in that case.
                self.agent_navigation.picker_refresh = None;
                self.refresh_agent_picker_threads(server, root);
            }
        }
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    pub(super) async fn forget_retired_agents(&mut self, thread_ids: &[ThreadId]) {
        let Some(root) = self.primary_thread_id else {
            return;
        };
        let ids = {
            let roster = self
                .agent_navigation
                .roster
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            thread_ids
                .iter()
                .copied()
                .filter(|id| {
                    !self.agent_navigation.is_retired(*id)
                        && (self.agent_navigation.get(id).is_some()
                            || roster.rows.iter().any(|row| row.id == *id))
                })
                .collect::<Vec<_>>()
        };
        if ids.is_empty() {
            return;
        }
        let return_to_root = self
            .current_displayed_thread_id()
            .is_some_and(|id| ids.contains(&id));
        for id in &ids {
            self.agent_navigation.retire(*id);
        }
        self.sync_subagent_roster();
        if return_to_root {
            self.app_event_tx.send(AppEvent::SelectAgentThread(root));
        }
        for id in ids {
            self.discard_thread_local_state(id).await;
        }
        self.app_event_tx
            .send(AppEvent::RefreshSubagentRoster(root));
    }
}
