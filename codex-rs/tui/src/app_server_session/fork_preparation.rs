use super::history::preparation::HistoryPaginationUpdate;
use super::session_preparation::{
    PreparedSession, RequestPolicy, SessionObservations, SubscriptionCustody, read_thread,
};
use super::*;
use codex_app_server_client::AppServerRequestHandle;

enum ForkPermissions {
    InheritSaved,
    CurrentConfig,
    Selected,
}

pub(crate) struct ForkPreparation {
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
}

impl AppServerSession {
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
        }
    }
}

impl ForkPreparation {
    pub(crate) async fn run(self) -> PreparedSession {
        let Self {
            handle,
            policy,
            parent,
            mut params,
            config,
            local_settings,
            mode,
            history_support,
            presentation,
            source,
            permissions,
            task_tools_available,
        } = self;
        let mut observations = SessionObservations {
            history: HistoryPaginationUpdate::Untracked,
            history_support,
        };
        let mut custody = SubscriptionCustody::None;
        let result = async {
            let fork_parent = match presentation {
                ForkPresentation::Regular => read_thread(policy, &handle, parent).await.ok(),
                ForkPresentation::SideConversation => None,
            };
            params.exclude_turns = history_support == ThreadHistorySupport::Paginated
                && (fork_parent
                    .as_ref()
                    .is_some_and(|thread| thread.history_mode == ThreadHistoryMode::Paginated)
                    || presentation == ForkPresentation::SideConversation);
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
                    request_id: RequestId::String(format!("tui-fork-{}", uuid::Uuid::new_v4())),
                    params: params.clone(),
                })
                .await
            {
                Ok(response) => response,
                Err(TypedRequestError::Server { source, .. })
                    if params.exclude_turns && is_history_pagination_unsupported(&source) =>
                {
                    observations.history_support = ThreadHistorySupport::LegacyOnly;
                    params.exclude_turns = false;
                    handle
                        .request_typed(ClientRequest::ThreadFork {
                            request_id: RequestId::String(format!(
                                "tui-fork-{}",
                                uuid::Uuid::new_v4()
                            )),
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
            if presentation == ForkPresentation::SideConversation {
                started.session.daybreak_enabled = false;
            }
            started.session.fork_parent_title = fork_parent.and_then(|thread| thread.name);
            if task_tools_available {
                started.task_tools_available = true;
            }
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
