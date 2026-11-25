//! Handle-only session preparation; observations return to the sole session owner.

use super::AppServerRequestHandle;
use super::AppServerSession;
use super::AppServerStartedThread;
use super::AskForApproval;
use super::ClientRequest;
use super::Config;
use super::HistoryHydrationScope;
use super::LocalSettings;
use super::PermissionProfileSelection;
use super::RequestId;
use super::Result;
use super::ResumeModelSettings;
use super::Thread;
use super::ThreadHistoryMode;
use super::ThreadHistorySupport;
use super::ThreadId;
use super::ThreadParamsMode;
use super::ThreadResumeParams;
use super::ThreadResumeResponse;
use super::ThreadStartParams;
use super::ThreadStartSource;
use super::TypedRequestError;
use super::Uuid;
use super::bootstrap_request_error;
use super::decode_thread_cwd;
use super::history;
use super::history::preparation::HistoryPaginationUpdate;
use super::is_history_pagination_unsupported;
use super::remove_permission_config_overrides;
use super::request_thread_start_with_history_fallback;
use super::started_thread_from_resume_response;
use super::started_thread_from_start_response;
use super::thread_resume_params_from_config;
use super::thread_session_state_from_thread_response;
use super::thread_start_params_from_config;
use codex_app_server_protocol::SortDirection;
use codex_app_server_protocol::ThreadTurnsListParams;
use codex_app_server_protocol::ThreadTurnsListResponse;
use codex_app_server_protocol::TurnItemsView;
use codex_utils_path_uri::LegacyAppPathString;
use codex_utils_path_uri::Platform;

pub(crate) async fn read_thread(
    policy: RequestPolicy,
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
) -> Result<Thread> {
    history::preparation::read_thread(policy, handle, thread_id, false).await
}

#[derive(Clone)]
pub(crate) struct ThreadMetadata {
    backend_id: crate::backend_id::BackendId,
    pub(crate) thread: Thread,
    receipt: Option<codex_app_server_protocol::ThreadReadState>,
}

pub(crate) async fn read_thread_with_receipt(
    policy: RequestPolicy,
    backend_id: crate::backend_id::BackendId,
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
) -> Result<ThreadMetadata> {
    let (thread, receipt) =
        history::preparation::read_thread_with_receipt(policy, handle, thread_id, false).await?;
    Ok(ThreadMetadata {
        backend_id,
        thread,
        receipt,
    })
}

pub(crate) async fn loaded_observations(
    policy: RequestPolicy,
    handle: &AppServerRequestHandle,
    root: ThreadId,
) -> Result<Vec<(ThreadId, Result<Thread>)>> {
    let response: codex_app_server_protocol::ThreadLoadedListResponse = request(
        policy,
        handle,
        ClientRequest::ThreadLoadedList {
            request_id: RequestId::String(format!("tui-discover-{}", Uuid::new_v4())),
            params: codex_app_server_protocol::ThreadLoadedListParams {
                cursor: None,
                limit: None,
            },
        },
    )
    .await?;
    let mut observations = Vec::new();
    for id in response.data {
        let thread_id = match ThreadId::from_string(&id) {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(%error, "ignoring invalid loaded thread id");
                continue;
            }
        };
        if thread_id != root {
            observations.push((thread_id, read_thread(policy, handle, thread_id).await));
        }
    }
    Ok(observations)
}

pub(crate) struct HydratedThread {
    pub(crate) thread: Thread,
    pub(crate) history: Result<()>,
}

pub(crate) struct PreparedHistory {
    result: Result<HydratedThread>,
    update: HistoryPaginationUpdate,
    backend_id: crate::backend_id::BackendId,
    receipt: Option<(ThreadId, Option<codex_app_server_protocol::ThreadReadState>)>,
}

impl PreparedHistory {
    pub(crate) fn apply(self, server: &mut AppServerSession) -> Result<HydratedThread> {
        color_eyre::eyre::ensure!(
            server.backend_id() == self.backend_id,
            "prepared history belongs to a different backend"
        );
        server.apply_history_preparation(self.update);
        if let Some((thread_id, receipt)) = self.receipt {
            server.remember_read_state(thread_id, receipt);
            server.freeze_viewed_receipt(thread_id);
        }
        self.result
    }
}

pub(crate) async fn prepare_thread_history(
    policy: RequestPolicy,
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    config: Config,
    local_settings: LocalSettings,
    image_origin: crate::history_cell::ImagePathOrigin,
    backend_id: crate::backend_id::BackendId,
) -> PreparedHistory {
    let mut update = HistoryPaginationUpdate::Untracked;
    let mut receipt = None;
    let result = async {
        let metadata = read_thread_with_receipt(policy, backend_id, &handle, thread_id).await?;
        receipt = Some((thread_id, metadata.receipt));
        let mut thread = metadata.thread;
        let (result, observation) = history::preparation::prepare_initial_thread_history(
            policy,
            &handle,
            &mut thread,
            None,
            None,
            Some(&config),
            Some(&local_settings),
            HistoryHydrationScope::Initial,
            image_origin,
        )
        .await;
        update = observation;
        Ok(HydratedThread {
            thread,
            history: result,
        })
    }
    .await;
    PreparedHistory {
        result,
        update,
        backend_id,
        receipt,
    }
}

#[derive(Clone, Copy)]
pub(crate) enum RequestPolicy {
    Unbounded,
    Interactive,
}

pub(crate) async fn request<T: serde::de::DeserializeOwned>(
    policy: RequestPolicy,
    handle: &AppServerRequestHandle,
    request: ClientRequest,
) -> std::result::Result<T, TypedRequestError> {
    if matches!(policy, RequestPolicy::Unbounded) {
        return handle.request_typed(request).await;
    }
    let method = request.method_name().to_string();
    tokio::time::timeout(
        crate::app::turn_submissions::ACK_TIMEOUT,
        handle.request_typed(request),
    )
    .await
    .unwrap_or_else(|_| {
        Err(TypedRequestError::Transport {
            method,
            source: std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "TUI preparation response deadline elapsed; the remote request may still complete",
            ),
        })
    })
}

pub(crate) enum ThreadStartChoice {
    Configured(Option<ThreadStartSource>),
    Provider(String),
}
pub(super) fn apply_selected_start_permissions(
    params: &mut ThreadStartParams,
    selected: Option<&PermissionProfileSelection>,
) {
    if let Some(selected) = selected {
        params.runtime_workspace_roots = None;
        params.permissions = Some(selected.profile_id.clone());
        params.sandbox = None;
        params.approval_policy = selected.approval_policy;
        params.approvals_reviewer = selected.approvals_reviewer.map(Into::into);
        remove_permission_config_overrides(&mut params.config);
    }
}

pub(crate) struct StartPreparation {
    backend_id: crate::backend_id::BackendId,
    handle: AppServerRequestHandle,
    params: ThreadStartParams,
    config: Config,
    local_settings: LocalSettings,
    mode: ThreadParamsMode,
    history_support: ThreadHistorySupport,
}
impl AppServerSession {
    pub(crate) fn prepare_start_thread(
        &self,
        local_settings: &LocalSettings,
        config: &Config,
        choice: ThreadStartChoice,
    ) -> StartPreparation {
        let params = match choice {
            ThreadStartChoice::Configured(source) => {
                let session_config = self.session_config_with_effective_service_tier(config);
                let mut params = thread_start_params_from_config(
                    &session_config,
                    self.thread_params_mode(),
                    self.remote_cwd_override.as_deref(),
                    source,
                );
                params.model_provider = self.explicit_model_provider(config);
                params.daybreak_enabled =
                    (config.daybreak_enabled && !config.ephemeral).then_some(true);
                params
            }
            ThreadStartChoice::Provider(provider) => {
                let mut params = thread_start_params_from_config(
                    config,
                    self.thread_params_mode(),
                    self.remote_cwd_override.as_deref(),
                    None,
                );
                params.model_provider = Some(provider);
                params
            }
        };
        self.prepare_start_with_params(local_settings, config, params)
    }
    pub(in crate::app_server_session) fn prepare_start_with_params(
        &self,
        local_settings: &LocalSettings,
        config: &Config,
        mut params: ThreadStartParams,
    ) -> StartPreparation {
        if self.history_support == ThreadHistorySupport::LegacyOnly {
            params.history_mode = None;
        }
        StartPreparation {
            backend_id: self.backend_id(),
            handle: self.request_handle(),
            params,
            config: config.clone(),
            local_settings: local_settings.clone(),
            mode: self.thread_params_mode(),
            history_support: self.history_support,
        }
    }
}
impl StartPreparation {
    pub(crate) fn with_selected_profile(
        mut self,
        selected: Option<&PermissionProfileSelection>,
    ) -> Self {
        apply_selected_start_permissions(&mut self.params, selected);
        self
    }

    pub(crate) async fn run(self) -> PreparedSession {
        let Self {
            backend_id,
            handle,
            params,
            config,
            local_settings,
            mode,
            history_support,
        } = self;
        let mut observations = SessionObservations {
            backend_id,
            history: HistoryPaginationUpdate::Untracked,
            history_support,
            receipt: None,
            subscribed: false,
            freeze_receipt: false,
            side_storage: None,
        };
        let mut custody = SubscriptionCustody::NewThreadUncertain;
        let selected_profile = params.permissions.is_some();
        let result = async {
            let request_id = RequestId::String(format!("tui-start-{}", Uuid::new_v4()));
            let (response, support, task_tools_available) =
                request_thread_start_with_history_fallback(&handle, request_id, params)
                    .await
                    .map_err(|error| {
                        if matches!(error, TypedRequestError::Server { .. }) {
                            custody = SubscriptionCustody::None;
                        }
                        bootstrap_request_error("thread/start failed during TUI bootstrap", error)
                    })?;
            let thread_id = ThreadId::from_string(&response.thread.id)?;
            custody = SubscriptionCustody::Subscribed(thread_id);
            if support == ThreadHistorySupport::LegacyOnly {
                observations.history_support = support;
            }
            let convention = Platform::from_platform_os(handle.platform_os()).path_convention();
            let cwd = decode_thread_cwd(&response.cwd, convention)?;
            let permissions = selected_profile
                .then(|| response.sandbox.permission_profile_for_cwd(&cwd))
                .transpose()?;
            let mut started =
                started_thread_from_start_response(response, &local_settings, &config, mode, cwd)
                    .await?;
            if let Some(permissions) = permissions {
                started.session.permission_profile = permissions;
            }
            started.task_tools_available = task_tools_available;
            Ok(started)
        }
        .await;
        PreparedSession {
            result,
            custody,
            observations,
        }
    }
}

pub(crate) struct ViewPreparation {
    backend_id: crate::backend_id::BackendId,
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    config: Config,
    local_settings: LocalSettings,
    policy: RequestPolicy,
    history_support: ThreadHistorySupport,
    image_origin: crate::history_cell::ImagePathOrigin,
}
pub(crate) struct PreparedView {
    pub(crate) result: Result<(AppServerStartedThread, Option<&'static str>)>,
    pub(crate) observations: SessionObservations,
}
impl AppServerSession {
    pub(crate) fn prepare_thread_view(
        &self,
        policy: RequestPolicy,
        config: &Config,
        local_settings: &LocalSettings,
        thread_id: ThreadId,
    ) -> ViewPreparation {
        ViewPreparation {
            backend_id: self.backend_id(),
            handle: self.request_handle(),
            thread_id,
            config: config.clone(),
            local_settings: local_settings.clone(),
            policy,
            history_support: self.history_support,
            image_origin: if self.uses_remote_workspace() {
                crate::history_cell::ImagePathOrigin::Remote
            } else {
                crate::history_cell::ImagePathOrigin::Local
            },
        }
    }
}
impl ViewPreparation {
    pub(crate) async fn run(self) -> PreparedView {
        let Self {
            backend_id,
            handle,
            thread_id,
            config,
            local_settings,
            policy,
            history_support,
            image_origin,
        } = self;
        let mut observations = SessionObservations {
            backend_id,
            history: HistoryPaginationUpdate::Untracked,
            history_support,
            receipt: None,
            subscribed: false,
            freeze_receipt: false,
            side_storage: None,
        };
        let result = async {
            let (mut thread, receipt) = history::preparation::read_thread_with_receipt(policy, &handle, thread_id, false).await?;
            observations.receipt = Some((thread_id, receipt));
            observations.freeze_receipt = true;
            let (history, update) = history::preparation::prepare_initial_thread_history(
                policy, &handle, &mut thread, None, None, Some(&config), Some(&local_settings), HistoryHydrationScope::Initial, image_origin,
            ).await;
            observations.history = update;
            let mut history_notice = None;
            if let Err(error) = history {
                if thread.history_mode == ThreadHistoryMode::Legacy { return Err(error); }
                tracing::warn!("Failed to page read-only thread history; trying summary pages");
                let page: ThreadTurnsListResponse = request(policy, &handle, ClientRequest::ThreadTurnsList {
                    request_id: RequestId::String(format!("tui-read-only-{}", Uuid::new_v4())),
                    params: ThreadTurnsListParams {
                        thread_id: thread_id.to_string(), cursor: None, limit: Some(100),
                        sort_direction: Some(SortDirection::Desc), items_view: Some(TurnItemsView::Summary),
                    },
                }).await?;
                thread.turns = page.data.into_iter().rev().collect();
                observations.history = HistoryPaginationUpdate::Summary(thread_id);
                history_notice = Some("Showing up to 100 recent prompts and final replies. Intermediate messages and tool activity are unavailable.");
            }
        let session = thread_session_state_from_thread_response(
            &thread.id,
            thread.environments.as_deref(),
            thread.forked_from_id.clone(),
            thread.name.clone(),
            thread.path.clone(),
            thread.model.clone().unwrap_or_default(),
            thread.model_provider.clone(),
            config.service_tier.clone(),
            config.openrouter_transport,
            AskForApproval::from(config.permissions.approval_policy.value()),
            config.approvals_reviewer,
            config.permissions.permission_profile().clone(),
            config.permissions.active_permission_profile(),
            decode_thread_cwd(
                &thread.cwd,
                Platform::from_platform_os(handle.platform_os()).path_convention(),
            )?,
            if image_origin == crate::history_cell::ImagePathOrigin::Local {
                LegacyAppPathString::from_abs_paths(&config.workspace_roots)
            } else {
                Vec::new()
            },
            Vec::new(),
            thread.reasoning_effort,
            &local_settings,
        )
        .await
        .map_err(color_eyre::eyre::Report::msg)?;
        Ok((
            AppServerStartedThread {
                can_accept_text_input: false,
                session,
                turns: thread.turns,
                blocks_direct_input: false,
                history_notice: None,
                task_tools_available: handle.supports_thread_coordination(),
            },
            history_notice,
        ))
        }.await;
        PreparedView {
            result,
            observations,
        }
    }
}

pub(crate) struct ResumePreparation {
    backend_id: crate::backend_id::BackendId,
    handle: AppServerRequestHandle,
    policy: RequestPolicy,
    thread_id: ThreadId,
    params: ThreadResumeParams,
    config: Config,
    local_settings: LocalSettings,
    mode: ThreadParamsMode,
    history_support: ThreadHistorySupport,
    known_legacy: bool,
    task_tools_available: bool,
    anchored: bool,
    metadata: Option<ThreadMetadata>,
}

pub(crate) enum SubscriptionCustody {
    None,
    Subscribed(ThreadId),
    Uncertain(ThreadId),
    NewThreadUncertain,
}

pub(crate) struct SessionObservations {
    pub(super) backend_id: crate::backend_id::BackendId,
    pub(super) history: HistoryPaginationUpdate,
    pub(super) history_support: ThreadHistorySupport,
    pub(super) receipt: Option<(ThreadId, Option<codex_app_server_protocol::ThreadReadState>)>,
    pub(super) subscribed: bool,
    pub(super) freeze_receipt: bool,
    pub(crate) side_storage: Option<super::SideChatStorage>,
}

pub(crate) struct SafetyHistoryPreparation {
    backend_id: crate::backend_id::BackendId,
    handle: AppServerRequestHandle,
    policy: RequestPolicy,
    thread_id: ThreadId,
    turn_id: String,
    history_support: ThreadHistorySupport,
}

pub(crate) struct PreparedSafetyHistory {
    pub(crate) result: Result<Thread>,
    pub(crate) observations: SessionObservations,
}

impl AppServerSession {
    pub(crate) fn prepare_safety_retry_history(
        &self,
        policy: RequestPolicy,
        thread_id: ThreadId,
        turn_id: String,
    ) -> SafetyHistoryPreparation {
        SafetyHistoryPreparation {
            backend_id: self.backend_id(),
            handle: self.request_handle(),
            policy,
            thread_id,
            turn_id,
            history_support: self.history_support,
        }
    }
}

impl SafetyHistoryPreparation {
    pub(crate) async fn run(self) -> PreparedSafetyHistory {
        PreparedSafetyHistory {
            result: history::preparation::prepare_safety_retry_history(
                self.policy,
                &self.handle,
                self.thread_id,
                &self.turn_id,
            )
            .await,
            observations: SessionObservations {
                backend_id: self.backend_id,
                history: HistoryPaginationUpdate::Untracked,
                history_support: self.history_support,
                receipt: None,
                subscribed: false,
                freeze_receipt: false,
                side_storage: None,
            },
        }
    }
}

pub(crate) struct PreparedSession {
    pub(crate) result: Result<AppServerStartedThread>,
    pub(crate) custody: SubscriptionCustody,
    pub(crate) observations: SessionObservations,
}

impl AppServerSession {
    pub(crate) fn prepare_resume_thread(
        &self,
        policy: RequestPolicy,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        model_settings: ResumeModelSettings,
        permission_overrides: crate::resume_permissions::ResumePermissions,
    ) -> ResumePreparation {
        let session_config = if matches!(
            model_settings,
            ResumeModelSettings::RestoreFromThread
                | ResumeModelSettings::PreserveExistingThread
                | ResumeModelSettings::PreserveSideConversation
        ) {
            config.clone()
        } else {
            self.session_config_with_effective_service_tier(&config)
        };
        let mut params = thread_resume_params_from_config(
            session_config,
            thread_id,
            self.thread_params_mode(),
            self.remote_cwd_override.as_deref(),
            model_settings,
            permission_overrides,
        );
        if model_settings == ResumeModelSettings::OverrideFromCurrentConfig {
            params.model_provider = self.explicit_model_provider(&config);
        }
        ResumePreparation {
            backend_id: self.backend_id(),
            handle: self.request_handle(),
            policy,
            thread_id,
            params,
            config,
            local_settings: local_settings.clone(),
            mode: self.thread_params_mode(),
            history_support: self.history_support,
            known_legacy: self
                .history_pagination
                .get(&thread_id)
                .is_some_and(|state| state.history_mode == ThreadHistoryMode::Legacy),
            task_tools_available: self.task_tools_available(thread_id),
            anchored: self.attachment_subscribed.is_some(),
            metadata: None,
        }
    }
    pub(crate) fn accepts_session_observations(&self, observations: &SessionObservations) -> bool {
        self.backend_id() == observations.backend_id
    }

    pub(crate) fn apply_session_observations(
        &mut self,
        observations: SessionObservations,
    ) -> Result<()> {
        color_eyre::eyre::ensure!(
            self.accepts_session_observations(&observations),
            "prepared session belongs to a different backend"
        );
        if observations.history_support == ThreadHistorySupport::LegacyOnly {
            self.history_support = ThreadHistorySupport::LegacyOnly;
        }
        self.apply_history_preparation(observations.history);
        if let Some((thread_id, receipt)) = observations.receipt {
            self.remember_read_state(thread_id, receipt);
            if observations.freeze_receipt {
                self.freeze_viewed_receipt(thread_id);
            }
        }
        if observations.subscribed
            && let Some(subscribed) = &mut self.attachment_subscribed
        {
            *subscribed = true;
        }
        Ok(())
    }
}

impl ResumePreparation {
    pub(crate) fn with_metadata(mut self, metadata: ThreadMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub(crate) async fn run(self) -> PreparedSession {
        let Self {
            backend_id,
            handle,
            policy,
            thread_id,
            mut params,
            config,
            local_settings,
            mode,
            history_support,
            known_legacy,
            task_tools_available,
            anchored,
            metadata,
        } = self;
        let mut observations = SessionObservations {
            backend_id,
            history: HistoryPaginationUpdate::Untracked,
            history_support,
            receipt: None,
            subscribed: false,
            freeze_receipt: false,
            side_storage: None,
        };
        let mut custody = SubscriptionCustody::None;
        let embedded = matches!(&handle, AppServerRequestHandle::InProcess(_));
        let result = Box::pin(async {
        if let Some(metadata) = &metadata {
            color_eyre::eyre::ensure!(metadata.backend_id == backend_id && metadata.thread.id == thread_id.to_string(), "resume metadata belongs to a different backend or thread");
        }
        let paginated = observations.history_support == ThreadHistorySupport::Paginated;
        let cached_legacy = paginated && known_legacy;
        let mut rollout_maintenance_guard = if cached_legacy && embedded {
            codex_rollout::try_acquire_rollout_maintenance_lock(config.codex_home.as_path()).ok().flatten()
        } else { None };
        observations.receipt = Some((thread_id, None));
        // Legacy maintenance needs a snapshot taken while its lock is held.
        let metadata = if cached_legacy && embedded {
            history::preparation::read_thread_with_receipt(policy, &handle, thread_id, false).await.ok()
        } else if let Some(metadata) = metadata {
            Some((metadata.thread, metadata.receipt))
        } else {
            history::preparation::read_thread_with_receipt(policy, &handle, thread_id, false).await.ok()
        };
        if let Some((_, receipt)) = &metadata { observations.receipt = Some((thread_id, receipt.clone())); }
        let known_legacy_history = cached_legacy && (!embedded || (rollout_maintenance_guard.is_some() && metadata.as_ref().is_some_and(|(thread, _)| thread.history_mode == ThreadHistoryMode::Legacy)));
        params.exclude_turns = paginated && !known_legacy_history;
        if params.exclude_turns {
            rollout_maintenance_guard = None;
        }
        let request_id = RequestId::String(format!("tui-resume-{}", Uuid::new_v4()));
        custody = SubscriptionCustody::Uncertain(thread_id);
        let resume_response = handle.request_typed(ClientRequest::ThreadResume {
                request_id,
                params: params.clone(),
            })
            .await;
        drop(rollout_maintenance_guard);
        let mut response: ThreadResumeResponse = match resume_response {
            Ok(response) => response,
            Err(TypedRequestError::Server { source, .. })
                if params.exclude_turns && is_history_pagination_unsupported(&source) =>
            {
                observations.history_support = ThreadHistorySupport::LegacyOnly;
                params.exclude_turns = false;
                let request_id = RequestId::String(format!("tui-resume-{}", Uuid::new_v4()));
                handle.request_typed(ClientRequest::ThreadResume { request_id, params })
                    .await
                    .map_err(|err| {
                        if matches!(err, TypedRequestError::Server { .. }) { custody = SubscriptionCustody::None; }
                        bootstrap_request_error("thread/resume failed during TUI bootstrap", err)
                    })?
            }
            Err(err) => {
                if matches!(err, TypedRequestError::Server { .. }) { custody = SubscriptionCustody::None; }
                return Err(bootstrap_request_error(
                    "thread/resume failed during TUI bootstrap",
                    err,
                ));
            }
        };
        custody = SubscriptionCustody::Subscribed(thread_id);
        observations.subscribed = true;
        let (history, update) = history::preparation::prepare_anchored_thread_history(policy, &handle,
                &mut response.thread,
                response.turns_backwards_cursor.clone(),
                response.items_backwards_cursor.clone(),
                Some(&config),
                Some(&local_settings),
                HistoryHydrationScope::Initial,
                anchored,
                if mode == ThreadParamsMode::Remote { crate::history_cell::ImagePathOrigin::Remote } else { crate::history_cell::ImagePathOrigin::Local },
            )
            .await;
        observations.history = update;
        let history_notice = match history {
            Ok(()) => None,
            Err(error) if response.thread.history_mode == ThreadHistoryMode::Paginated => {
                Some(format!("Conversation history is incomplete: {error:#}"))
            }
            Err(error) => return Err(error),
        };
        let fork_parent_title = match response.thread.forked_from_id.as_deref().and_then(|id| ThreadId::from_string(id).ok()) {
            Some(parent) => match history::preparation::read_thread(policy, &handle, parent, false).await {
                Ok(thread) => thread.name,
                Err(error) => { tracing::warn!(%error, "Failed to read fork parent metadata from app server"); None }
            },
            None => None,
        };
        let mut started = started_thread_from_resume_response(
            response,
            &local_settings,
            Platform::from_platform_os(handle.platform_os()).path_convention(),
        )
        .await?;
        started.history_notice = history_notice;
        started.session.fork_parent_title = fork_parent_title;
        if task_tools_available {
            started.task_tools_available = true;
        }
        Ok(started)
        }).await;
        PreparedSession {
            result,
            custody,
            observations,
        }
    }
}
