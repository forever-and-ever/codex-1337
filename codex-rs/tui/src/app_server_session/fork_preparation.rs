use super::AppServerSession;
use super::ClientRequest;
use super::Config;
use super::ForkConfigSource;
use super::ForkGoalContinuation;
use super::ForkName;
use super::ForkPermissionMode;
use super::ForkPresentation;
use super::HistoryHydrationScope;
use super::LocalSettings;
use super::PermissionProfile;
use super::PermissionProfileSelection;
use super::RequestId;
use super::SideChatStorage;
use super::ThreadForkParams;
use super::ThreadForkResponse;
use super::ThreadHistoryMode;
use super::ThreadHistorySupport;
use super::ThreadId;
use super::ThreadParamsMode;
use super::TypedRequestError;
use super::Uuid;
use super::bootstrap_request_error;
use super::history;
use super::history::preparation::HistoryPaginationUpdate;
use super::is_history_pagination_unsupported;
use super::remove_permission_config_overrides;
use super::session_preparation;
use super::session_preparation::PreparedSession;
use super::session_preparation::RequestPolicy;
use super::session_preparation::SessionObservations;
use super::session_preparation::SubscriptionCustody;
use super::session_preparation::read_thread;
use super::started_thread_from_fork_response;
use super::thread_blocks_direct_input;
use super::thread_fork_params_from_config;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ThreadInjectItemsParams;
use codex_app_server_protocol::ThreadInjectItemsResponse;

enum ForkPermissions {
    InheritSaved,
    CurrentConfig,
    Selected,
}

pub(crate) struct ForkPreparation {
    backend_id: crate::backend_id::BackendId,
    handle: AppServerRequestHandle,
    policy: RequestPolicy,
    parent: ThreadId,
    params: ThreadForkParams,
    config: Config,
    local_settings: LocalSettings,
    mode: ThreadParamsMode,
    history_support: ThreadHistorySupport,
    presentation: ForkPresentation,
    source: ForkConfigSource,
    permissions: ForkPermissions,
    task_tools_available: bool,
    uses_remote_workspace: bool,
}

impl AppServerSession {
    pub(crate) fn prepare_side_thread(
        &self,
        policy: RequestPolicy,
        local_settings: &LocalSettings,
        config: Config,
        parent: ThreadId,
        selected_profile: Option<&PermissionProfileSelection>,
    ) -> ForkPreparation {
        self.prepare_fork_with_presentation(
            policy,
            local_settings,
            config,
            parent,
            None,
            None,
            ForkGoalContinuation::StartIfIdle,
            ForkPresentation::SideConversation(SideChatStorage::Temporary),
            selected_profile,
            ForkPermissionMode::InheritSaved,
            ForkConfigSource::Session,
            None,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "fork position and named permissions are independent"
    )]
    pub(crate) fn prepare_fork_thread(
        &self,
        policy: RequestPolicy,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        last_turn_id: Option<String>,
        before_turn_id: Option<String>,
        goal_continuation: ForkGoalContinuation,
        selected_profile: Option<&PermissionProfileSelection>,
        name: Option<ForkName>,
    ) -> ForkPreparation {
        self.prepare_fork_with_presentation(
            policy,
            local_settings,
            config,
            thread_id,
            last_turn_id,
            before_turn_id,
            goal_continuation,
            ForkPresentation::Regular,
            selected_profile,
            ForkPermissionMode::InheritSaved,
            ForkConfigSource::Session,
            name,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "fork presentation and permission provenance are independent"
    )]
    pub(super) fn prepare_fork_with_presentation(
        &self,
        policy: RequestPolicy,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        last_turn_id: Option<String>,
        before_turn_id: Option<String>,
        goal_continuation: ForkGoalContinuation,
        presentation: ForkPresentation,
        selected_profile: Option<&PermissionProfileSelection>,
        permission_mode: ForkPermissionMode,
        source: ForkConfigSource,
        name: Option<ForkName>,
    ) -> ForkPreparation {
        let session_config = if config.model.is_none() {
            config.clone()
        } else {
            self.session_config_with_effective_service_tier(&config)
        };
        let mut params = ThreadForkParams {
            snapshot: match presentation {
                ForkPresentation::Regular => {
                    codex_app_server_protocol::ThreadForkSnapshot::Interrupted
                }
                ForkPresentation::SideConversation(_) => {
                    codex_app_server_protocol::ThreadForkSnapshot::Side
                }
            },
            name: name.as_ref().map(|name| name.name.clone()),
            conflict_thread_ids: name.map(|name| name.conflict_thread_ids),
            last_turn_id,
            before_turn_id,
            defer_goal_continuation: goal_continuation == ForkGoalContinuation::DeferUntilNextTurn,
            ..thread_fork_params_from_config(
                session_config,
                thread_id,
                self.thread_params_mode(),
                self.remote_cwd_override.as_deref(),
            )
        };
        params.model_provider = match source {
            ForkConfigSource::Local => self.explicit_model_provider(&config),
            ForkConfigSource::Session => Some(config.model_provider_id.clone()),
        };
        if source == ForkConfigSource::Session {
            // Fork does not restore omitted roots; the active config adopted the server's roots.
            params.runtime_workspace_roots = Some(config.workspace_roots.clone());
        }
        let permissions = if let Some(selected) = selected_profile {
            params.runtime_workspace_roots = None;
            params.permissions = Some(selected.profile_id.clone());
            params.sandbox = None;
            params.approval_policy = selected.approval_policy;
            params.approvals_reviewer = selected.approvals_reviewer.map(Into::into);
            remove_permission_config_overrides(&mut params.config);
            ForkPermissions::Selected
        } else {
            match permission_mode {
                ForkPermissionMode::InheritSaved => ForkPermissions::InheritSaved,
                ForkPermissionMode::OverrideFromCurrentConfig => ForkPermissions::CurrentConfig,
            }
        };
        ForkPreparation {
            backend_id: self.backend_id(),
            handle: self.request_handle(),
            policy,
            parent: thread_id,
            params,
            config,
            local_settings: local_settings.clone(),
            mode: self.thread_params_mode(),
            history_support: self.history_support,
            presentation,
            source,
            permissions,
            task_tools_available: self.task_tools_available(thread_id),
            uses_remote_workspace: self.uses_remote_workspace(),
        }
    }
}

impl ForkPreparation {
    pub(crate) fn with_storage(mut self, storage: SideChatStorage) -> Self {
        if matches!(self.presentation, ForkPresentation::SideConversation(_)) {
            self.presentation = ForkPresentation::SideConversation(storage);
        }
        self
    }

    pub(crate) async fn run(self) -> PreparedSession {
        let Self {
            backend_id,
            handle,
            policy,
            parent,
            mut params,
            mut config,
            local_settings,
            mode,
            history_support,
            mut presentation,
            source,
            permissions,
            task_tools_available,
            uses_remote_workspace,
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
        let result = Box::pin(async {
            let base_developer_instructions = config.developer_instructions.clone();
            let mut boundary_items = Vec::new();
            if let ForkPresentation::SideConversation(mut storage) = presentation {
                if storage == SideChatStorage::Attached {
                    let supported = if uses_remote_workspace { false } else {
                        let mut view = AppServerSession::from_parts(crate::backend_id::BackendId::new(), None, handle.clone(), mode);
                        view.attached_side_chat_query(parent).await?.is_some()
                            && read_thread(policy, &handle, parent).await.is_ok_and(|thread| !thread.ephemeral && thread.history_mode == ThreadHistoryMode::Paginated)
                    };
                    if !supported { storage = SideChatStorage::Temporary; }
                }
                presentation = ForkPresentation::SideConversation(storage);
                storage.configure_fork(&mut config);
                params.ephemeral = config.ephemeral;
                params.developer_instructions = config.developer_instructions.clone();
                params.attach_to_source = storage == SideChatStorage::Attached;
                if params.attach_to_source {
                    params.thread_source = Some(codex_app_server_protocol::ThreadSource::Feature(super::ATTACHED_SIDE_CHAT_SOURCE.to_owned()));
                }
                // Inherited turn context can suppress configured instructions; checkpoint both policy and boundary.
                boundary_items = storage.policy().fork_items();
                observations.side_storage = Some(storage);
            }
            let fork_parent = match presentation {
                ForkPresentation::Regular => read_thread(policy, &handle, parent).await.ok(),
                ForkPresentation::SideConversation(_) => None,
            };
            params.exclude_turns = history_support == ThreadHistorySupport::Paginated
                && (fork_parent
                    .as_ref()
                    .is_some_and(|thread| thread.history_mode == ThreadHistoryMode::Paginated)
                    || matches!(presentation, ForkPresentation::SideConversation(_)));
            let child_fork = source == ForkConfigSource::Session
                && fork_parent.as_ref().is_some_and(thread_blocks_direct_input);
            if child_fork {
                params.cwd = Some(config.cwd.to_string_lossy().into_owned());
                // A child's saved tier must not be inferred from the client's default model.
                params.service_tier = Some(config.service_tier.clone());
                params.base_instructions = None;
                params.developer_instructions = None;
            }
            if (child_fork || mode == ThreadParamsMode::Remote)
                && matches!(permissions, ForkPermissions::InheritSaved)
            {
                params.approval_policy = None;
                params.approvals_reviewer = None;
                params.sandbox = None;
                params.permissions = None;
                remove_permission_config_overrides(&mut params.config);
            }
            // Retain the acquisition future: a cancelled wait cannot prove that no fork exists.
            custody = SubscriptionCustody::NewThreadUncertain;
            let response: ThreadForkResponse = match handle
                .request_typed(ClientRequest::ThreadFork {
                    request_id: RequestId::String(format!("tui-fork-{}", Uuid::new_v4())),
                    params: params.clone(),
                })
                .await
            {
                Ok(response) => response,
                Err(TypedRequestError::Server { source, .. })
                    if params.attach_to_source && source.code == super::JSONRPC_METHOD_NOT_FOUND
                        && source.message == "attached threads is not supported yet" =>
                {
                    params.attach_to_source = false;
                    params.thread_source = None;
                    params.ephemeral = true;
                    params.developer_instructions = Some(SideChatStorage::Temporary.policy().instructions(base_developer_instructions.as_deref()));
                    presentation = ForkPresentation::SideConversation(SideChatStorage::Temporary);
                    observations.side_storage = Some(SideChatStorage::Temporary);
                    boundary_items = SideChatStorage::Temporary.policy().fork_items();
                    handle.request_typed(ClientRequest::ThreadFork {
                        request_id: RequestId::String(format!("tui-fork-{}", Uuid::new_v4())), params: params.clone(),
                    }).await.map_err(|error| {
                        if matches!(error, TypedRequestError::Server { .. }) { custody = SubscriptionCustody::None; }
                        bootstrap_request_error("thread/fork failed during TUI bootstrap", error)
                    })?
                }
                Err(TypedRequestError::Server { source, .. })
                    if params.exclude_turns && !params.attach_to_source && is_history_pagination_unsupported(&source) =>
                {
                    observations.history_support = ThreadHistorySupport::LegacyOnly;
                    params.exclude_turns = false;
                    handle
                        .request_typed(ClientRequest::ThreadFork {
                            request_id: RequestId::String(format!("tui-fork-{}", Uuid::new_v4())),
                            params,
                        })
                        .await
                        .map_err(|error| {
                            if matches!(error, TypedRequestError::Server { .. }) {
                                custody = SubscriptionCustody::None;
                            }
                            bootstrap_request_error(
                                "thread/fork failed during TUI bootstrap",
                                error,
                            )
                        })?
                }
                Err(error) => {
                    if matches!(error, TypedRequestError::Server { .. }) {
                        custody = SubscriptionCustody::None;
                    }
                    return Err(bootstrap_request_error(
                        "thread/fork failed during TUI bootstrap",
                        error,
                    ));
                }
            };
            custody = SubscriptionCustody::Subscribed(ThreadId::from_string(&response.thread.id)?);
            if !boundary_items.is_empty() {
                let child = ThreadId::from_string(&response.thread.id)?;
                let items = boundary_items
                    .into_iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<_>, _>>()?;
                let injected: Result<ThreadInjectItemsResponse, TypedRequestError> =
                    session_preparation::request(
                        policy,
                        &handle,
                        ClientRequest::ThreadInjectItems {
                            request_id: RequestId::String(format!(
                                "tui-side-boundary-{}",
                                Uuid::new_v4()
                            )),
                            params: ThreadInjectItemsParams {
                                thread_id: child.to_string(),
                                items,
                            },
                        },
                    )
                    .await;
                if let Err(error) = injected {
                    if !matches!(error, TypedRequestError::Server { .. }) {
                        custody = SubscriptionCustody::Uncertain(child);
                    }
                    if presentation == ForkPresentation::SideConversation(SideChatStorage::Attached) {
                        let rollback: Result<codex_app_server_protocol::ThreadDeleteResponse, TypedRequestError> = session_preparation::request(policy, &handle, ClientRequest::ThreadDelete {
                            request_id: RequestId::String(format!("tui-side-rollback-{}", Uuid::new_v4())),
                            params: codex_app_server_protocol::ThreadDeleteParams { thread_id: child.to_string() },
                        }).await;
                        if rollback.is_ok() { custody = SubscriptionCustody::None; }
                        else { tracing::warn!(%child, ?rollback, "failed to roll back incomplete attached side conversation"); }
                    }
                    return Err(bootstrap_request_error(
                        "side conversation boundary could not be confirmed",
                        error,
                    ));
                }
            }
            if presentation == ForkPresentation::SideConversation(SideChatStorage::Attached) {
                let child = ThreadId::from_string(&response.thread.id)?;
                let checkpoint = super::side_chat_history::side_context_checkpoint(response.thread.path.as_deref()).await;
                if !matches!(checkpoint, Ok(Some(_))) {
                    let rollback = handle.request_typed::<codex_app_server_protocol::ThreadDeleteResponse>(ClientRequest::ThreadDelete {
                        request_id: RequestId::String(format!("tui-side-rollback-{}", Uuid::new_v4())),
                        params: codex_app_server_protocol::ThreadDeleteParams { thread_id: child.to_string() },
                    }).await;
                    if rollback.is_ok() { custody = SubscriptionCustody::None; }
                    color_eyre::eyre::bail!("Saved side conversation context was not confirmed; checkpoint={checkpoint:?}, rollback={rollback:?}");
                }
            }
            let mut response = response;
            if presentation == ForkPresentation::Regular && !response.thread.ephemeral {
                let (result, update) = history::preparation::prepare_initial_thread_history(
                    policy,
                    &handle,
                    &mut response.thread,
                    None,
                    None,
                    Some(&config),
                    Some(&local_settings),
                    HistoryHydrationScope::Initial,
                    if uses_remote_workspace { crate::history_cell::ImagePathOrigin::Remote } else { crate::history_cell::ImagePathOrigin::Local },
                )
                .await;
                observations.history = update;
                if let Err(error) = result {
                    tracing::warn!(
                        thread_id = %response.thread.id,
                        error = %error,
                        "preserving the created fork after history hydration failed"
                    );
                }
            }
            let selected_permissions =
                matches!(permissions, ForkPermissions::Selected).then(|| {
                    PermissionProfile::from_legacy_sandbox_policy_for_cwd(
                        &response.sandbox.to_core(),
                        response.cwd.as_path(),
                    )
                });
            let mut started =
                started_thread_from_fork_response(response, &local_settings, &config, mode).await?;
            if let Some(permissions) = selected_permissions {
                started.session.permission_profile = permissions;
            }
            if matches!(presentation, ForkPresentation::SideConversation(_)) {
                started.session.daybreak_enabled = false;
            }
            started.session.fork_parent_title = fork_parent.and_then(|thread| thread.name);
            if task_tools_available {
                started.task_tools_available = true;
            }
            Ok(started)
        })
        .await;
        PreparedSession {
            result,
            custody,
            observations,
        }
    }
}

#[cfg(test)]
impl AppServerSession {
    pub(crate) async fn fork_side_thread(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        selected_profile: Option<&PermissionProfileSelection>,
        storage: SideChatStorage,
    ) -> color_eyre::Result<super::AppServerStartedThread> {
        let prepared = Box::pin(
            self.prepare_side_thread(
                RequestPolicy::Unbounded,
                local_settings,
                config,
                thread_id,
                selected_profile,
            )
            .with_storage(storage)
            .run(),
        )
        .await;
        self.apply_session_observations(prepared.observations)?;
        prepared.result
    }
}
