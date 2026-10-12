//! Handle-only session preparation; observations return to the sole session owner.

use super::history::preparation::HistoryPaginationUpdate;
use super::*;

pub(crate) async fn read_thread(
    policy: RequestPolicy,
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
) -> Result<Thread> {
    history::preparation::read_thread(policy, handle, thread_id, false).await
}

pub(crate) struct HydratedThread {
    pub(crate) thread: Thread,
    pub(crate) history: Result<()>,
}

pub(crate) struct PreparedHistory {
    result: Result<HydratedThread>,
    update: HistoryPaginationUpdate,
}

impl PreparedHistory {
    pub(crate) fn apply(self, server: &mut AppServerSession) -> Result<HydratedThread> {
        server.apply_history_preparation(self.update);
        self.result
    }
}

pub(crate) async fn prepare_thread_history(
    policy: RequestPolicy,
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    config: Config,
    local_settings: LocalSettings,
) -> PreparedHistory {
    let mut update = HistoryPaginationUpdate::Untracked;
    let result = async {
        let mut thread = read_thread(policy, &handle, thread_id).await?;
        let (result, observation) = history::preparation::prepare_initial_thread_history(
            policy,
            &handle,
            &mut thread,
            None,
            None,
            Some(&config),
            Some(&local_settings),
            HistoryHydrationScope::Initial,
        )
        .await;
        update = observation;
        Ok(HydratedThread {
            thread,
            history: result,
        })
    }
    .await;
    PreparedHistory { result, update }
}

#[derive(Clone, Copy)]
pub(crate) enum RequestPolicy {
    Unbounded,
    Interactive,
}

pub(in crate::app_server_session) async fn request<T: serde::de::DeserializeOwned>(
    policy: RequestPolicy,
    handle: &codex_app_server_client::AppServerRequestHandle,
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

pub(crate) struct ResumePreparation {
    handle: AppServerRequestHandle,
    policy: RequestPolicy,
    thread_id: ThreadId,
    params: ThreadResumeParams,
    config: Config,
    local_settings: LocalSettings,
    mode: ThreadParamsMode,
    history_support: ThreadHistorySupport,
    known_legacy: bool,
    embedded: bool,
    task_tools_available: bool,
}

pub(crate) enum SubscriptionCustody {
    None,
    Subscribed(ThreadId),
    Uncertain(ThreadId),
}

pub(crate) struct SessionObservations {
    history: HistoryPaginationUpdate,
    history_support: ThreadHistorySupport,
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
        local_settings: &crate::local_settings::LocalSettings,
        config: Config,
        thread_id: ThreadId,
        model_settings: ResumeModelSettings,
        permission_overrides: crate::resume_permissions::ResumePermissions,
    ) -> ResumePreparation {
        let session_config = if matches!(
            model_settings,
            ResumeModelSettings::RestoreFromThread | ResumeModelSettings::PreserveExistingThread
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
            embedded: self.uses_embedded_app_server(),
            task_tools_available: self.task_tools_available(thread_id),
        }
    }
    pub(crate) fn apply_session_observations(&mut self, observations: SessionObservations) {
        if observations.history_support == ThreadHistorySupport::LegacyOnly {
            self.history_support = ThreadHistorySupport::LegacyOnly;
        }
        self.apply_history_preparation(observations.history);
    }
}

impl ResumePreparation {
    pub(crate) async fn run(self) -> PreparedSession {
        let Self {
            handle,
            policy,
            thread_id,
            mut params,
            config,
            local_settings,
            mode,
            history_support,
            known_legacy,
            embedded,
            task_tools_available,
        } = self;
        let mut observations = SessionObservations {
            history: HistoryPaginationUpdate::Untracked,
            history_support,
        };
        let mut custody = SubscriptionCustody::None;
        let result = async {
        let mut rollout_maintenance_guard = None;
        params.exclude_turns = if observations.history_support == ThreadHistorySupport::Paginated {
            let known_legacy_history = known_legacy
                && (!embedded
                    || ({
                        // The guard prevents migration through the full resume,
                        // regardless of the server's migration feature settings.
                        rollout_maintenance_guard =
                            codex_rollout::try_acquire_rollout_maintenance_lock(
                                config.codex_home.as_path(),
                            )
                            .ok()
                            .flatten();
                        rollout_maintenance_guard.is_some()
                    } && history::preparation::read_thread(policy, &handle, thread_id, false)
                        .await
                        .is_ok_and(|thread| thread.history_mode == ThreadHistoryMode::Legacy)));
            !known_legacy_history
        } else {
            false
        };
        if params.exclude_turns {
            rollout_maintenance_guard = None;
        }
        let request_id = RequestId::String(format!("tui-resume-{}", uuid::Uuid::new_v4()));
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
                let request_id = RequestId::String(format!("tui-resume-{}", uuid::Uuid::new_v4()));
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
        let (history, update) = history::preparation::prepare_initial_thread_history(policy, &handle,
                &mut response.thread,
                response.turns_backwards_cursor.clone(),
                response.items_backwards_cursor.clone(),
                Some(&config),
                Some(&local_settings),
                HistoryHydrationScope::Initial,
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
            mode,
        )
        .await?;
        started.history_notice = history_notice;
        started.session.fork_parent_title = fork_parent_title;
        if task_tools_available {
            started.task_tools_available = true;
        }
        Ok(started)
        }.await;
        PreparedSession {
            result,
            custody,
            observations,
        }
    }
}
