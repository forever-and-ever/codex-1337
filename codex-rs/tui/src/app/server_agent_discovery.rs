//! Discovery returns metadata observations; only matching navigation intent may project them.
use super::AbortOnDropHandle;
use super::AgentNavigationDirection;
use super::App;
use super::AppServerSession;
use super::Feature;
use super::FutureExt;
use super::LaneState;
use super::NavigationCompletion;
use super::NavigationIntent;
use super::NavigationReceipt;
use super::PendingNavigation;
use super::Result;
use super::ServerCompletion;
use super::ThreadEventAttachment;
use super::ThreadId;
use super::agent_status_feed;
use super::find_loaded_subagent_threads_for_primary;
use super::tui;
use crate::app_server_session::session_preparation::RequestPolicy;
use codex_app_server_protocol as api;

use crate::app::agent_picker::AGENT_PICKER_VIEW_ID;

pub(super) enum DiscoveryIntent {
    Picker(Vec<ThreadId>),
    Adjacent {
        root: ThreadId,
        from: Option<ThreadId>,
        direction: AgentNavigationDirection,
    },
}

pub(super) enum DiscoveryReceipt {
    Liveness(Vec<(ThreadId, Result<api::Thread>)>),
    Descendants(Result<Vec<(ThreadId, Result<api::Thread>)>>),
    Terminated,
}

impl App {
    pub(in crate::app) fn enqueue_agent_picker_discovery(&mut self, server: &AppServerSession) {
        let mut ids = self.agent_navigation.tracked_thread_ids();
        for id in self.thread_event_channels.keys().copied() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let path_backed = self
            .agent_navigation
            .ordered_path_backed_subagent_threads(self.primary_thread_id)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        ids.retain(|id| {
            !(self.side_threads.contains_key(id)
                || path_backed.contains(id)
                    && self
                        .thread_event_channels
                        .get(id)
                        .is_some_and(|channel| channel.attachment() == ThreadEventAttachment::Live))
        });
        self.enqueue_navigation(
            server,
            NavigationIntent::Discover(DiscoveryIntent::Picker(ids)),
        );
    }

    async fn select_adjacent_thread(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> Result<()> {
        if self.side_retention_enabled(tui) {
            self.select_agent_thread(tui, server, thread_id).await
        } else {
            self.select_agent_thread_and_discard_side(tui, server, thread_id)
                .await
        }
    }

    pub(in crate::app) async fn navigate_adjacent_thread(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        direction: AgentNavigationDirection,
    ) -> Result<()> {
        let from = self.current_displayed_thread_id();
        if let Some(id) = self.agent_navigation.adjacent_thread_id(from, direction) {
            return self.select_adjacent_thread(tui, server, id).await;
        }
        let Some(root) = self.primary_thread_id else {
            return Ok(());
        };
        if self.last_subagent_backfill_attempt != Some(root) {
            self.enqueue_navigation(
                server,
                NavigationIntent::Discover(DiscoveryIntent::Adjacent {
                    root,
                    from,
                    direction,
                }),
            );
        }
        Ok(())
    }

    pub(super) fn dispatch_discovery(
        &self,
        server: &AppServerSession,
        operation: &mut PendingNavigation,
    ) {
        let plan = match &operation.intent {
            NavigationIntent::Discover(DiscoveryIntent::Picker(ids)) => {
                DiscoveryIntent::Picker(ids.clone())
            }
            NavigationIntent::Discover(DiscoveryIntent::Adjacent {
                root,
                from,
                direction,
            }) => DiscoveryIntent::Adjacent {
                root: *root,
                from: *from,
                direction: *direction,
            },
            _ => unreachable!("discovery dispatch requires its typed intent"),
        };
        let handle = server.request_handle();
        let completions = self.server_operations.control_tx.clone();
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            let receipt = std::panic::AssertUnwindSafe(async {
                match plan {
                    DiscoveryIntent::Picker(ids) => {
                        let mut observations = Vec::new();
                        for id in ids {
                            observations.push((
                                id,
                                crate::app_server_session::session_preparation::read_thread(
                                    RequestPolicy::Interactive,
                                    &handle,
                                    id,
                                )
                                .await,
                            ));
                        }
                        DiscoveryReceipt::Liveness(observations)
                    }
                    DiscoveryIntent::Adjacent { root, .. } => DiscoveryReceipt::Descendants(
                        crate::app_server_session::session_preparation::loaded_observations(
                            RequestPolicy::Interactive,
                            &handle,
                            root,
                        )
                        .await,
                    ),
                }
            })
            .catch_unwind()
            .await
            .unwrap_or(DiscoveryReceipt::Terminated);
            let _ = completions.send(ServerCompletion::Navigation(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::Discovered(receipt),
                },
            )));
        })));
    }

    pub(super) async fn finish_discovery(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        operation: PendingNavigation,
        receipt: DiscoveryReceipt,
    ) -> Result<()> {
        match (operation.intent, receipt) {
            (
                NavigationIntent::Discover(DiscoveryIntent::Picker(_)),
                DiscoveryReceipt::Liveness(observations),
            ) => {
                for (id, result) in observations {
                    self.apply_thread_liveness(result, id);
                }
                self.show_agent_picker(server);
            }
            (
                NavigationIntent::Discover(DiscoveryIntent::Adjacent {
                    root,
                    from,
                    direction,
                }),
                DiscoveryReceipt::Descendants(result),
            ) => {
                if self.primary_thread_id != Some(root)
                    || self.current_displayed_thread_id() != from
                {
                    return Ok(());
                }
                match result {
                    Ok(observations) => {
                        if self.apply_loaded_subagent_observations(root, observations) {
                            self.last_subagent_backfill_attempt = Some(root);
                        }
                        if let Some(id) = self.agent_navigation.adjacent_thread_id(from, direction)
                        {
                            self.select_adjacent_thread(tui, server, id).await?;
                        }
                    }
                    Err(error) => tracing::warn!(%error, "failed to discover adjacent agents"),
                }
            }
            (_, DiscoveryReceipt::Terminated) => {
                self.add_agents_overview_error("Agent discovery terminated unexpectedly.".into());
            }
            _ => unreachable!("discovery family received a different receipt"),
        }
        Ok(())
    }

    #[expect(
        clippy::needless_collect,
        reason = "Snapshot channel identifiers before mutably borrowing App to seed picker entries."
    )]
    pub(in crate::app) fn show_agent_picker(&mut self, app_server: &AppServerSession) {
        let path_backed_threads = self
            .agent_navigation
            .ordered_path_backed_subagent_threads(self.primary_thread_id);
        if !path_backed_threads.is_empty() {
            let running_threads: Vec<_> = path_backed_threads
                .into_iter()
                .filter_map(|(thread_id, entry)| {
                    if !matches!(
                        self.agent_navigation.status(thread_id),
                        Some(codex_app_server_protocol::ThreadStatus::Active { .. })
                    ) {
                        return None;
                    }
                    Some((thread_id, entry.agent_path.as_deref()?.trim().to_string()))
                })
                .collect();
            let mut entries = Vec::new();
            for (thread_id, agent_path) in running_threads {
                let preview = if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                    match channel.store.try_lock() {
                        Ok(store) => agent_status_feed::AgentStatusThreadPreview::from_store(
                            agent_path, &store,
                        ),
                        Err(_) => agent_status_feed::AgentStatusThreadPreview::empty(agent_path),
                    }
                } else {
                    agent_status_feed::AgentStatusThreadPreview::empty(agent_path)
                };
                entries.push(preview);
            }

            self.chat_widget
                .add_to_history(agent_status_feed::AgentStatusHistoryCell::new(entries));
        }

        for thread_id in self
            .thread_event_channels
            .keys()
            .copied()
            .collect::<Vec<_>>()
        {
            if !self.side_threads.contains_key(&thread_id)
                && self.agent_navigation.get(&thread_id).is_none()
            {
                self.upsert_agent_picker_thread(thread_id, None, None, false);
            }
        }
        let has_non_primary_agent_thread = self
            .agent_navigation
            .has_non_primary_thread(self.primary_thread_id);
        if !self.config.features.enabled(Feature::Collab) && !has_non_primary_agent_thread {
            if let Some(primary_thread_id) = self.primary_thread_id {
                self.refresh_agent_picker_threads(app_server, primary_thread_id);
            }
            self.chat_widget.open_feature_enable_prompt(Feature::Collab);
            return;
        }

        if self.agent_navigation.is_empty() {
            self.chat_widget
                .add_info_message("No agents available yet.".to_string(), /*hint*/ None);
            return;
        }

        let selected = self
            .chat_widget
            .selected_index_for_present_view(AGENT_PICKER_VIEW_ID)
            .and_then(|index| self.agent_navigation.picker_row_ids.get(index).copied());
        let params = self.agent_picker_selection_view_params(selected);
        if !self
            .chat_widget
            .replace_selection_view_if_present(AGENT_PICKER_VIEW_ID, params)
        {
            let params = self.agent_picker_selection_view_params(selected);
            self.chat_widget.show_selection_view(params);
        }
        if let Some(primary_thread_id) = self.primary_thread_id {
            self.refresh_agent_picker_threads(app_server, primary_thread_id);
        }
    }
    pub(in crate::app) fn apply_loaded_subagent_observations(
        &mut self,
        primary_thread_id: ThreadId,
        observations: Vec<(ThreadId, Result<codex_app_server_protocol::Thread>)>,
    ) -> bool {
        let had_read_error = observations.iter().any(|(_, result)| result.is_err());
        let threads = observations
            .into_iter()
            .filter_map(|(thread_id, result)| match result {
                Ok(thread) => Some(thread),
                Err(error) => {
                    tracing::warn!(%thread_id, %error, "failed to read loaded thread");
                    None
                }
            })
            .collect();
        for thread in find_loaded_subagent_threads_for_primary(threads, primary_thread_id) {
            if let Some(model) = thread.model {
                self.agent_navigation.models.insert(thread.thread_id, model);
            } else {
                self.agent_navigation.models.remove(&thread.thread_id);
            }
            let agent_path = thread.agent_path;
            let is_closed = matches!(
                thread.status,
                codex_app_server_protocol::ThreadStatus::NotLoaded
            );
            if thread.blocks_direct_input {
                self.agent_navigation.mark_parent_owned(thread.thread_id);
            }
            self.upsert_agent_picker_thread(
                thread.thread_id,
                thread.agent_nickname,
                thread.agent_role,
                is_closed,
            );
            if self.agent_navigation.get(&thread.thread_id).is_some() {
                self.agent_navigation
                    .descendant_roots
                    .insert(thread.thread_id, primary_thread_id);
            }
            self.agent_navigation
                .set_agent_path(thread.thread_id, agent_path);
            self.agent_navigation
                .set_status(thread.thread_id, thread.status);
        }
        self.sync_active_agent_label();

        !had_read_error
    }
}
