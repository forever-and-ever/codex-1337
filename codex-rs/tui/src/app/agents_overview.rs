//! Overview of recent and locally retained sessions and their subagents.
//! Tasks owned by another app server open as frozen, read-only history snapshots.
//! Only the immediate attachment of a dashboard-created task is treated as fresh.

#[path = "agents_overview_trust.rs"]
pub(in crate::app) mod trust;

#[path = "agents_overview_new.rs"]
mod new;

#[path = "agents_overview_errors.rs"]
mod errors;

#[path = "agents_overview_selection.rs"]
mod selection;

use super::agents_overview_view::AgentsOverviewGroup;
use super::agents_overview_view::AgentsOverviewRow;
use super::agents_overview_view::AgentsOverviewView;
use super::app_server_requests::BufferedServerRequest;
use super::session_lifecycle::ThreadAttachPresentation;
use super::*;
use crate::app_event::AgentsOverviewThreadRefresh;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::chatwidget::ThreadInputStateRestoreMode;
use crate::startup_draft::StartupDraftPump;
use codex_app_server_protocol::SessionSource;
use codex_app_server_protocol::Thread;
use codex_protocol::protocol::SubAgentSource;

pub(crate) const AGENTS_OVERVIEW_VIEW_ID: &str = "agents-overview";

#[derive(Default)]
pub(super) struct AgentsOverviewState {
    /// Missing metadata records a local resume until the next metadata refresh.
    pub(super) threads: HashMap<ThreadId, Option<Thread>>,
    /// Identifies the query that populated the cached search results.
    pub(super) search_id: Uuid,
    pub(super) search_matches: HashMap<ThreadId, Thread>,
    /// Lifecycle removals take precedence over delayed tool registration responses.
    pub(super) removed_threads: HashSet<ThreadId>,
    /// Local visibility only; activity and metadata refreshes never reveal hidden roots.
    pub(super) hidden_threads: HashSet<ThreadId>,
    pub(super) unread: HashSet<ThreadId>,
    pub(super) last_messages: HashMap<ThreadId, String>,
    pub(super) usage: HashMap<ThreadId, super::agents_overview_usage::AgentsOverviewUsage>,
    pub(super) pending_usage: Option<(ThreadId, Uuid)>,
    pub(super) usage_disabled: bool,
    pub(super) activity: HashMap<ThreadId, super::agents_overview_details::AgentsOverviewActivity>,
    pub(super) initialized: bool,
    pub(super) discovery: super::agents_overview_discovery::AgentsOverviewDiscovery,
    pub(super) show_more_requested: bool,
    pub(super) refresh_show_more: bool,
    /// Vacancies left by lifecycle removals, filled without expanding the visible window.
    pub(super) refill_count: usize,
    pub(super) request_id: Option<Uuid>,
    pub(super) refresh_pending: bool,
    pub(super) pin_refresh_requested: bool,
    pub(super) refresh_thread_ids: HashSet<ThreadId>,
    pub(super) active_refresh_thread_ids: HashSet<ThreadId>,
    pub(super) pinned_thread_ids: Option<Vec<ThreadId>>,
    pub(super) pending_pin_change: Option<Uuid>,
    pub(super) refresh_task: Option<tokio::task::AbortHandle>,
    pub(super) refresh_notifications: HashMap<ThreadId, Vec<ServerNotification>>,
    pub(super) rendered_full_screen: bool,
    pub(super) visible_thread_ids: Vec<ThreadId>,
    /// One-shot successor selected before a removal invalidates the displayed rows.
    pub(super) selection_after_removal: Option<ThreadId>,
    pub(super) view_state:
        Arc<std::sync::Mutex<super::agents_overview_view::AgentsOverviewViewState>>,
    /// Explicit permission-profile choices for new-session carryover, retained across navigation.
    pub(super) selected_permission_profiles: HashMap<ThreadId, String>,
    /// Accepted menu requests; unchanged settings may never produce a notification.
    pub(super) requested_permission_profiles: HashMap<ThreadId, PermissionProfileSelection>,
    /// Keep new tasks subscribed and reusable until a first turn makes them resumable.
    pub(super) blank_sessions: HashMap<ThreadId, crate::app_server_session::AppServerStartedThread>,
    pub(super) input_states: HashMap<ThreadId, ThreadInputState>,
    pub(super) new_session_draft: Option<Box<StartupDraftPump>>,
    pub(super) parked_draft: Option<Box<super::session_draft::ParkedSessionDraft>>,
    pub(super) dispatched_requests: HashMap<ThreadId, Vec<BufferedServerRequest>>,
}

impl Drop for AgentsOverviewState {
    fn drop(&mut self) {
        if let Some(task) = self.refresh_task.take() {
            task.abort();
        }
    }
}

impl App {
    pub(super) fn open_agents_overview(&mut self, app_server: &AppServerSession) {
        let threads = self
            .agents_overview
            .threads
            .values()
            .flatten()
            .cloned()
            .collect();
        let selected_thread_id = self.agents_overview.selection_after_removal.take();
        let view = self.agents_overview_view(threads, selected_thread_id);
        self.agents_overview.visible_thread_ids = view.thread_ids();
        self.chat_widget.show_bottom_pane_view(Box::new(view));
        self.resolve_runtime_theme();
        self.sync_frontend_requests();
        if self.reconnect.offline {
            self.reconnect.presentation = reconnect::ReconnectPresentation::Overview;
            let mut state = self
                .agents_overview
                .view_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.loading = false;
            state.connection_notice = Some(if self.reconnect.failed {
                "Reconnect failed — agent list is stale; relaunch to retry"
            } else {
                "Reconnecting — agent list is stale"
            });
        } else {
            self.refresh_agents_overview_threads(app_server);
        }
    }

    pub(super) fn apply_agents_overview_thread_refresh(
        &mut self,
        app_server: &AppServerSession,
        request_id: Uuid,
        result: Result<AgentsOverviewThreadRefresh, String>,
    ) {
        if self.agents_overview.request_id != Some(request_id) {
            return;
        }
        self.agents_overview.request_id = None;
        self.agents_overview.refresh_show_more = false;
        self.agents_overview.active_refresh_thread_ids.clear();
        self.agents_overview.refresh_task = None;
        let refill_succeeded = result
            .as_ref()
            .is_ok_and(|refresh| refresh.recent_seed_complete);
        {
            let mut state = self
                .agents_overview
                .view_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.loading = false;
            state.refresh_failed = !refill_succeeded;
        }
        match result {
            Ok(refresh) => {
                for id in refresh.cleared_unread {
                    self.agents_overview.unread.remove(&id);
                }
                if let Some(pinned_thread_ids) = refresh.pinned_thread_ids {
                    self.agents_overview.pinned_thread_ids = pinned_thread_ids;
                }
                self.agents_overview.initialized = refresh.recent_seed_complete;
                if let Some(discovery) = refresh.discovery {
                    if !discovery.has_more() {
                        self.agents_overview.refill_count = 0;
                        self.agents_overview.show_more_requested = false;
                    }
                    self.agents_overview.initialized = true;
                    self.agents_overview
                        .view_state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .has_more = discovery.has_more();
                    self.agents_overview.discovery = discovery;
                }
                self.agents_overview
                    .last_messages
                    .extend(refresh.last_messages);
                for (thread_id, thread) in refresh.threads {
                    if self.agents_overview.removed_threads.contains(&thread_id) {
                        continue;
                    }
                    if let Some((mut thread, unread, latest_prompt)) = thread {
                        self.agents_overview.search_matches.remove(&thread_id);
                        if unread {
                            self.agents_overview.unread.insert(thread_id);
                        } else {
                            self.agents_overview.unread.remove(&thread_id);
                        }
                        if thread.ephemeral {
                            self.agents_overview.threads.remove(&thread_id);
                            self.agents_overview.last_messages.remove(&thread_id);
                            self.agents_overview.activity.remove(&thread_id);
                            self.agents_overview.usage.remove(&thread_id);
                            continue;
                        }
                        if !self.agents_overview.threads.contains_key(&thread_id)
                            && !self.agents_overview.hidden_threads.contains(&thread_id)
                            && thread.parent_thread_id.is_none()
                            && !matches!(
                                thread.source,
                                SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
                            )
                        {
                            self.agents_overview.refill_count =
                                self.agents_overview.refill_count.saturating_sub(1);
                        }
                        self.agents_overview
                            .activity
                            .entry(thread_id)
                            .or_default()
                            .latest_prompt = latest_prompt;
                        thread.turns.clear();
                        self.agents_overview.threads.insert(thread_id, Some(thread));
                    } else {
                        self.agents_overview.threads.entry(thread_id).or_default();
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to refresh agents overview");
            }
        }
        for notifications in
            std::mem::take(&mut self.agents_overview.refresh_notifications).into_values()
        {
            for notification in notifications {
                if let ServerNotification::ThreadReverted(reverted) = &notification
                    && let Ok(thread_id) = ThreadId::from_string(&reverted.thread_id)
                {
                    // Discard stale read results without clearing activity received after the revert.
                    if let Some(activity) = self.agents_overview.activity.get_mut(&thread_id) {
                        activity.latest_prompt = None;
                    }
                    self.agents_overview.last_messages.remove(&thread_id);
                    if let Some(usage) = self.agents_overview.usage.get_mut(&thread_id) {
                        usage.tokens = None;
                    }
                    continue;
                }
                self.track_agents_overview_notification(&notification);
            }
        }
        if std::mem::take(&mut self.agents_overview.refresh_pending)
            || (refill_succeeded
                && (self.agents_overview.refill_count > 0
                    || self.agents_overview.show_more_requested)
                && self.agents_overview.discovery.has_more())
        {
            self.refresh_changed_agents_overview_threads(app_server);
        }
        self.repaint_agents_overview();
    }

    pub(super) fn repaint_agents_overview(&mut self) {
        let Some(selected) = self
            .chat_widget
            .selected_index_for_present_view(AGENTS_OVERVIEW_VIEW_ID)
        else {
            return;
        };
        let previous_thread_id = self
            .agents_overview
            .visible_thread_ids
            .get(selected)
            .copied();
        let selected_thread_id = self
            .agents_overview
            .view_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rename_target
            .filter(|id| {
                self.agents_overview.threads.contains_key(id)
                    && !self.agents_overview.hidden_threads.contains(id)
            })
            .or(self.agents_overview.selection_after_removal.take())
            .or(previous_thread_id);
        let threads = self
            .agents_overview
            .threads
            .values()
            .flatten()
            .cloned()
            .collect();
        let view = self.agents_overview_view(threads, selected_thread_id);
        let matches = &mut self.agents_overview.search_matches;
        if !matches.is_empty() {
            let visible: HashSet<_> = view
                .visible_indices()
                .into_iter()
                .map(|i| view.rows[i].thread_id)
                .collect();
            matches.retain(|id, _| {
                visible.contains(id) || self.agents_overview.hidden_threads.contains(id)
            });
        }
        view.load_search_matches(/*additional*/ 0);
        self.agents_overview.visible_thread_ids = view.thread_ids();
        let next_thread_id = view
            .selected_index()
            .and_then(|index| self.agents_overview.visible_thread_ids.get(index).copied());
        if (previous_thread_id.is_none() || previous_thread_id != next_thread_id)
            && self
                .chat_widget
                .selected_index_for_active_view(AGENTS_OVERVIEW_VIEW_ID)
                .is_some()
        {
            self.cancel_pending_key_chord();
        }
        if let Ok(mut state) = self.agents_overview.view_state.lock()
            && state
                .rename_target
                .is_some_and(|id| !self.agents_overview.visible_thread_ids.contains(&id))
        {
            self.chat_widget.add_info_message(
                format!(
                    "The rename target disappeared. Unsubmitted title: {}",
                    state.input.text()
                ),
                /*hint*/ None,
            );
            state.rename_target = None;
            state.input.set_text_clearing_elements("");
        }
        self.chat_widget
            .replace_bottom_pane_view_if_present(AGENTS_OVERVIEW_VIEW_ID, Box::new(view));
    }

    pub(super) fn agents_overview_view(
        &self,
        mut threads: Vec<Thread>,
        selected_thread_id: Option<ThreadId>,
    ) -> AgentsOverviewView {
        {
            let overview = &self.agents_overview;
            let mut state = overview
                .view_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.vim_enabled = self.chat_widget.composer_is_vim_enabled();
            if state
                .history
                .as_ref()
                .is_some_and(|search| search.id == overview.search_id)
            {
                let query = state.search.to_lowercase();
                threads.extend(
                    overview
                        .search_matches
                        .values()
                        .filter(|thread| {
                            super::agents_overview_discovery::matches_query(thread, &query)
                        })
                        .cloned(),
                );
            } else if let Some(target) = state
                .rename_target
                .and_then(|id| overview.search_matches.get(&id))
            {
                threads.push(target.clone());
            }
        }
        threads.retain(|thread| !thread.ephemeral);
        for thread in &mut threads {
            if thread.parent_thread_id.is_none()
                && let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                    parent_thread_id, ..
                }) = &thread.source
            {
                thread.parent_thread_id = Some(parent_thread_id.to_string());
            }
        }
        let mut children: HashMap<String, Vec<&Thread>> = HashMap::new();
        for thread in &threads {
            if let Some(parent_thread_id) = &thread.parent_thread_id {
                children
                    .entry(parent_thread_id.clone())
                    .or_default()
                    .push(thread);
            }
        }

        let voice_owner = self.voice_owner_thread_id().map(|id| id.to_string());
        let voice_session = threads
            .iter()
            .find(|thread| Some(&thread.id) == voice_owner.as_ref())
            .map(|thread| &thread.session_id);
        let mut roots = threads
            .iter()
            .filter(|thread| thread.parent_thread_id.is_none())
            .map(|root| (root, agents_overview_group(root, &children)))
            .collect::<Vec<_>>();
        roots.sort_by(|(left, left_group), (right, right_group)| {
            left_group
                .cmp(right_group)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut rows = Vec::new();
        for (root, group) in roots {
            let Ok(thread_id) = ThreadId::from_string(&root.id) else {
                continue;
            };
            if self.agents_overview.hidden_threads.contains(&thread_id) {
                continue;
            }
            rows.push(AgentsOverviewRow {
                search_id: self
                    .agents_overview
                    .search_matches
                    .contains_key(&thread_id)
                    .then_some(self.agents_overview.search_id),
                unread: self.agents_overview.unread.contains(&thread_id),
                details: self.agents_overview_details(root, &children),
                thread: root.clone(),
                thread_id,
                group,
                is_current: self.primary_thread_id == Some(thread_id),
                has_voice: voice_session == Some(&root.session_id),
            });
        }

        let mut view = AgentsOverviewView::new(
            rows,
            selected_thread_id,
            self.local_settings.tui.status_line_use_colors,
            self.app_event_tx.clone(),
            self.keymap.clone(),
            Arc::clone(&self.agents_overview.view_state),
        );
        view.pinned_thread_ranks = self.agents_overview.pinned_thread_ids.as_ref().map(|ids| {
            ids.iter()
                .copied()
                .enumerate()
                .map(|(rank, id)| (id, rank))
                .collect()
        });
        view.can_return_to_chat =
            self.pending_startup_thread_start || self.current_displayed_thread_id().is_some();
        view.pin_action_pending = self.agents_overview.pending_pin_change.is_some();
        view
    }

    pub(super) async fn select_agents_overview_thread(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> Result<AppRunControl> {
        self.select_agents_overview_thread_after(
            tui,
            app_server,
            thread_id,
            super::turn_submissions::SelectionAfter::SelectOnly,
        )
        .await
    }

    async fn attach_new_agents_overview_thread(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        root_thread_id: ThreadId,
        started: (Config, crate::app_server_session::AppServerStartedThread),
    ) -> color_eyre::Result<AppRunControl> {
        let has_authoritative_mode = started.1.session.collaboration_mode.is_some();
        if self.windows_sandbox_blocks_thread_switch() {
            return Ok(AppRunControl::Continue);
        }

        if self.current_displayed_thread_id() == Some(root_thread_id)
            && (!self.thread_unavailable(root_thread_id) || self.chat_widget.is_read_only_view())
        {
            // Keep the displayed read-only snapshot frozen; R explicitly retries attachment.
            if let Ok(mut state) = self.agents_overview.view_state.lock() {
                state.completion = Some(crate::bottom_pane::ViewCompletion::Accepted);
            }
            self.chat_widget.pre_draw_tick();
            return Ok(AppRunControl::Continue);
        }
        if self.reject_pending_permission_root_switch() {
            return Ok(AppRunControl::Continue);
        }
        if self.primary_thread_id != Some(root_thread_id) || self.thread_unavailable(root_thread_id)
        {
            let previous_displayed_thread_id = self.current_displayed_thread_id();
            if let Some(id) = previous_displayed_thread_id
                && let Some(blank) = self.agents_overview.blank_sessions.get_mut(&id)
                && let Some(channel) = self.thread_event_channels.get(&id)
                && let Some(session) = channel.store.lock().await.session.as_ref()
            {
                blank.session = session.clone();
            }
            let mut previous_thread_ids =
                Vec::from_iter(self.thread_event_channels.keys().copied());
            previous_thread_ids.extend(
                self.agent_navigation
                    .tracked_thread_ids()
                    .into_iter()
                    .filter(|thread_id| !self.thread_event_channels.contains_key(thread_id)),
            );
            let mut previous_running_thread_ids = Vec::new();
            let mut previous_pending_requests = Vec::new();
            for thread_id in &previous_thread_ids {
                if self.side_threads.contains_key(thread_id) {
                    continue;
                }
                if let Some(channel) = self.thread_event_channels.get(thread_id) {
                    let store = channel.store.lock().await;
                    if store.active_turn_id().is_some() {
                        previous_running_thread_ids.push(*thread_id);
                    }
                    if let Some(input_state) = store.input_state.clone() {
                        self.agents_overview
                            .input_states
                            .insert(*thread_id, input_state);
                    }
                    let requests: Vec<_> = store
                        .pending_replay_requests()
                        .into_iter()
                        .filter_map(|request| {
                            self.pending_app_server_requests.buffered_request(request)
                        })
                        .collect();
                    if !requests.is_empty() {
                        previous_pending_requests.push((*thread_id, requests));
                    }
                } else if self
                    .agent_navigation
                    .get(thread_id)
                    .is_some_and(|entry| entry.is_running)
                {
                    previous_running_thread_ids.push(*thread_id);
                }
            }
            if let Some(active_thread_id) = self.current_displayed_thread_id()
                && let Some(input_state) = self.chat_widget.capture_thread_input_state()
            {
                self.agents_overview
                    .input_states
                    .insert(active_thread_id, input_state);
            }

            let (resume_config, resumed) = started;
            let local_settings = self.local_settings.reloaded(&resume_config);
            let baseline_approval = resume_config.permissions.approval_policy.value();
            let baseline_reviewer = resume_config.approvals_reviewer;
            let baseline_permissions =
                RuntimePermissionProfileOverride::from_config(&resume_config);
            let presentation = ThreadAttachPresentation::FreshWithDraft;
            for (thread_id, requests) in previous_pending_requests {
                self.agents_overview
                    .dispatched_requests
                    .entry(thread_id)
                    .or_default()
                    .extend(requests);
            }
            if !previous_running_thread_ids.is_empty() {
                for thread_id in &previous_thread_ids {
                    if !self.side_threads.contains_key(thread_id) {
                        self.agents_overview
                            .dispatched_requests
                            .entry(*thread_id)
                            .or_default();
                    }
                }
            }
            if previous_running_thread_ids.is_empty() {
                let mut detach = Vec::new();
                if !previous_displayed_thread_id
                    .is_some_and(|id| self.agents_overview.blank_sessions.contains_key(&id))
                {
                    detach.extend(self.thread_event_channels.keys().copied().filter(|id| {
                        *id != root_thread_id
                            && self.voice_owner_thread_id() != Some(*id)
                            && !self.side_threads.contains_key(id)
                            && !self.agents_overview.dispatched_requests.contains_key(id)
                            && !self.agents_overview.blank_sessions.contains_key(id)
                    }));
                }
                detach.extend(previous_thread_ids.iter().copied().filter(|id| {
                    *id != root_thread_id
                        && self.voice_owner_thread_id() != Some(*id)
                        && Some(*id) != previous_displayed_thread_id
                        && !self.side_threads.contains_key(id)
                        && !self.agents_overview.blank_sessions.contains_key(id)
                }));
                self.unsubscribe_overview_threads(app_server, detach);
            }
            self.runtime_approval_policy_override = self
                .runtime_approval_policy_override
                .filter(|policy| matches!(policy, RuntimeApprovalPolicyOverride::Explicit(_)));
            self.runtime_permission_profile_override = self
                .runtime_permission_profile_override
                .take()
                .filter(|profile| {
                    profile.turn_override == RuntimePermissionProfileTurnOverride::LegacySandbox
                });
            self.local_settings = local_settings;
            self.refresh_server_version_overview_notice(CODEX_CLI_VERSION);
            self.config = resume_config;
            tui.set_notification_settings(&self.local_settings.tui.notification_settings);
            self.file_search
                .update_search_dir(self.config.cwd.to_path_buf());
            if let Err(error) = self
                .replace_chat_widget_with_app_server_thread(
                    tui,
                    resumed,
                    presentation,
                    /*initial_user_message*/ None,
                )
                .await
            {
                self.add_agents_overview_error(format!("Failed to attach to task: {error}"));
                return Ok(AppRunControl::Continue);
            }
            let mut destination_config = self.chat_widget.config_ref().clone();
            if self.app_server_target.uses_remote_workspace() {
                destination_config.cwd.clone_from(&self.config.cwd);
                destination_config
                    .workspace_roots
                    .clone_from(&self.config.workspace_roots);
                destination_config
                    .permissions
                    .set_workspace_roots(self.config.permissions.workspace_roots().to_vec());
            }

            self.config = destination_config;

            self.remember_launch_permissions();

            let approval = self.config.permissions.approval_policy.value();
            if self
                .runtime_approval_policy_override
                .is_none_or(|policy| policy.policy().to_core() != approval)
            {
                self.runtime_approval_policy_override = (approval != baseline_approval)
                    .then_some(RuntimeApprovalPolicyOverride::Restored(approval.into()));
            }
            if self
                .runtime_permission_profile_override
                .as_ref()
                .is_none_or(|profile| !profile.matches_config(&self.config))
            {
                self.runtime_permission_profile_override = (!baseline_permissions
                    .matches_config(&self.config)
                    || baseline_reviewer != self.config.approvals_reviewer)
                    .then(|| RuntimePermissionProfileOverride::from_restored_config(&self.config));
            }

            // A new session has no descendants. Scanning every loaded thread here
            // adds a serial round trip per agent before the composer can render.
        }

        if self.current_displayed_thread_id() != Some(root_thread_id)
            || (self.thread_unavailable(root_thread_id) && !self.chat_widget.is_read_only_view())
        {
            self.select_agent_thread(tui, app_server, root_thread_id)
                .await?;
        }
        if self.current_displayed_thread_id() == Some(root_thread_id) {}
        let read_only = self.chat_widget.is_read_only_view();
        if !read_only {
            self.replay_agents_overview_requests(app_server, root_thread_id)
                .await;
        }
        if self.current_displayed_thread_id() == Some(root_thread_id)
            && let Some(input_state) = self.agents_overview.input_states.remove(&root_thread_id)
        {
            let preserve_in_flight_turn = self
                .active_turn_id_for_thread(root_thread_id)
                .await
                .is_some();
            let accepted_mode =
                has_authoritative_mode.then(|| self.chat_widget.effective_collaboration_mode());
            self.chat_widget.restore_bound_thread_input_state(
                Some(input_state),
                ThreadInputStateRestoreMode {
                    preserve_in_flight_turn,
                },
                crate::chatwidget::ThreadInputSettingsAuthority::Session { accepted_mode },
            );

            if !preserve_in_flight_turn {
                self.chat_widget.maybe_send_next_queued_input();
            }
        }

        self.queue_farm_receipt_replay();
        Ok(AppRunControl::Continue)
    }

    pub(super) async fn replay_agents_overview_requests(
        &mut self,
        app_server: &AppServerSession,
        root_thread_id: ThreadId,
    ) {
        let requests = self
            .agents_overview
            .dispatched_requests
            .extract_if(|id, _| *id == root_thread_id || self.agent_navigation.get(id).is_some())
            .flat_map(|(_, requests)| requests)
            .collect::<Vec<_>>();
        for request in requests {
            self.handle_server_request_event(app_server, request).await;
        }
    }

    async fn agents_overview_session_config(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        cwd: Option<AbsolutePathBuf>,
        startup_draft: &mut StartupDraftPump,
    ) -> Option<trust::DashboardConfigOutcome> {
        if self
            .chat_widget
            .thread_id()
            .is_some_and(|thread_id| self.pending_server_profiles.contains_key(&thread_id))
        {
            self.add_agents_overview_error(
                "Wait for permissions to update before starting a session.".into(),
            );
            return None;
        }
        let remote = app_server.uses_remote_workspace();
        let remote_cwd = cwd
            .as_ref()
            .filter(|_| remote)
            .map(AbsolutePathBuf::to_path_buf);
        let local_cwd = if remote {
            self.config.cwd.to_path_buf()
        } else {
            cwd.map_or_else(
                || self.chat_widget.config_ref().cwd.to_path_buf(),
                |cwd| cwd.to_path_buf(),
            )
        };
        let config = match StartupDraftPump::run_with_optional_draft(
            Some(&mut *startup_draft),
            tui,
            self.rebuild_config_for_cwd(local_cwd),
        )
        .await
        {
            Ok(config) => config,
            Err(error) => {
                self.add_agents_overview_error(format!("Failed to load project settings: {error}"));
                return None;
            }
        };
        self.prepare_pumped_dashboard_trust(
            tui,
            app_server,
            trust::DashboardTrustStage::Initial(trust::DashboardPreparation { config, remote_cwd }),
            startup_draft,
        )
        .await
    }

    pub(in crate::app) async fn finish_dashboard_session_config(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        preparation: trust::DashboardPreparation,
        startup_draft: &mut StartupDraftPump,
    ) -> Option<(Config, Option<PathBuf>)> {
        let trust::DashboardPreparation {
            mut config,
            remote_cwd,
        } = preparation;
        if let Some(profile) = self.runtime_permission_profile_override.as_ref()
            && profile.turn_override == RuntimePermissionProfileTurnOverride::LegacySandbox
            && profile
                .active_permission_profile
                .as_ref()
                .is_some_and(|active| !active.id.starts_with(':'))
            && self.chat_widget.thread_id().is_none_or(|thread_id| {
                self.agents_overview
                    .selected_permission_profiles
                    .get(&thread_id)
                    != profile
                        .active_permission_profile
                        .as_ref()
                        .map(|active| &active.id)
            })
            && (!profile.matches_config(&config)
                || config.permissions.profile_workspace_roots()
                    != self.config.permissions.profile_workspace_roots())
        {
            self.add_agents_overview_error(
                "Permission profile has different settings.".to_string(),
            );
            return None;
        }
        let defaults_cwd = match app_server.thread_params_mode() {
            crate::app_server_session::ThreadParamsMode::Embedded => config.cwd.as_path(),
            crate::app_server_session::ThreadParamsMode::Remote => remote_cwd
                .as_deref()
                .or_else(|| app_server.remote_cwd_override())
                .unwrap_or(Path::new(".")),
        }
        .to_path_buf();
        startup_draft.apply_settings(
            &crate::local_settings::LocalSettings::from(&config),
            config.cwd.as_path(),
        );
        let mut server_model_cleared = false;
        match StartupDraftPump::run_with_optional_draft(
            Some(startup_draft),
            tui,
            crate::config_update::read_effective_config_if_supported(
                app_server.request_handle(),
                &defaults_cwd,
            ),
        )
        .await
        {
            Ok(Some(defaults)) => {
                crate::projectless::apply_defaults(
                    &mut config,
                    &self.harness_overrides,
                    app_server,
                    &self.environment_manager,
                    &defaults,
                );
                let defaults = defaults.config;
                server_model_cleared = defaults.model.is_none();
                let use_server_provider = matches!(
                    app_server.thread_params_mode(),
                    crate::app_server_session::ThreadParamsMode::Embedded
                ) && self.harness_overrides.model.is_none()
                    && !super::new_session::has_launch_setting(
                        &config,
                        &self.cli_kv_overrides,
                        "model",
                    )
                    && self.harness_overrides.model_provider.is_none()
                    && !super::new_session::has_launch_setting(
                        &config,
                        &self.cli_kv_overrides,
                        "model_provider",
                    );
                super::new_session::overlay_new_session_defaults(
                    &mut config,
                    &defaults,
                    &self.cli_kv_overrides,
                    &self.harness_overrides,
                );
                // Embedded thread/start sends a provider ID alongside the selected model.
                if use_server_provider {
                    config.model_provider_id = defaults
                        .model_provider
                        .unwrap_or_else(|| "openai".to_string());
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.add_agents_overview_error(format!(
                    "Failed to load new session settings: {error}"
                ));
                return None;
            }
        }
        // New sessions inherit explicit choices, not permissions restored from another task.
        if let Err(error) = self
            .apply_runtime_policy_overrides(&mut config, RuntimePolicyOverrideScope::ExplicitOnly)
        {
            self.add_agents_overview_error(format!("{error:#}"));
            return None;
        }
        apply_managed_new_thread_defaults(
            &mut config,
            app_server.managed_new_thread_defaults(),
            &self.cli_kv_overrides,
            &self.harness_overrides,
        );
        if server_model_cleared
            && config.model.is_none()
            && (config.features.enabled(Feature::FastMode)
                || config.features.enabled(Feature::UltrafastMode))
        {
            // Bootstrap's fallback model may be seeded from the client. Resolve tiers
            // against the server catalog when config/read cleared the model.
            config.model = self
                .model_catalog
                .models
                .iter()
                .find(|model| model.is_default)
                .or_else(|| self.model_catalog.models.first())
                .map(|model| model.model.clone());
        }
        Some((config, remote_cwd))
    }

    pub(super) fn stop_agents_overview_thread(
        &mut self,
        app_server: &AppServerSession,
        thread_id: ThreadId,
    ) {
        self.enqueue_server_operation(
            app_server,
            thread_id,
            super::turn_submissions::ServerOperation::Control(
                super::turn_submissions::ControlOperation::InterruptActive {
                    paginated_history: app_server.supports_paginated_history(),
                },
            ),
        );
    }
}

#[cfg(test)]
#[path = "agents_overview_tests.rs"]
pub(super) mod tests;

fn agents_overview_group(
    thread: &Thread,
    children: &HashMap<String, Vec<&Thread>>,
) -> AgentsOverviewGroup {
    children
        .get(&thread.id)
        .into_iter()
        .flatten()
        .fold(AgentsOverviewGroup::for_thread(thread), |group, child| {
            group.min(agents_overview_group(child, children))
        })
}
