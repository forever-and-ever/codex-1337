//! Test-only helpers shared across the TUI crate.

use std::sync::LazyLock;

use codex_models_manager::bundled_models_response;
use codex_protocol::openai_models::ModelPreset;
pub(crate) use codex_utils_absolute_path::test_support::PathBufExt;
pub(crate) use codex_utils_absolute_path::test_support::test_path_buf;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) async fn thread_goal_get(
    server: &crate::app_server_session::AppServerSession,
    thread_id: codex_protocol::ThreadId,
) -> color_eyre::Result<codex_app_server_protocol::ThreadGoalGetResponse> {
    use codex_app_server_protocol::ClientRequest;
    use codex_app_server_protocol::RequestId;
    use codex_app_server_protocol::ThreadGoalGetParams;
    Ok(server
        .request_handle()
        .request_typed(ClientRequest::ThreadGoalGet {
            request_id: RequestId::String(format!("test-goal-read-{}", uuid::Uuid::new_v4())),
            params: ThreadGoalGetParams {
                thread_id: thread_id.to_string(),
            },
        })
        .await?)
}

pub(crate) async fn thread_goal_set(
    server: &crate::app_server_session::AppServerSession,
    thread_id: codex_protocol::ThreadId,
    objective: Option<String>,
    status: Option<codex_app_server_protocol::ThreadGoalStatus>,
    token_budget: Option<Option<i64>>,
) -> color_eyre::Result<codex_app_server_protocol::ThreadGoalSetResponse> {
    use codex_app_server_protocol::ClientRequest;
    use codex_app_server_protocol::RequestId;
    use codex_app_server_protocol::ThreadGoalMutationOrigin;
    use codex_app_server_protocol::ThreadGoalSetParams;
    Ok(server
        .request_handle()
        .request_typed(ClientRequest::ThreadGoalSet {
            request_id: RequestId::String(format!("test-goal-set-{}", uuid::Uuid::new_v4())),
            params: ThreadGoalSetParams {
                thread_id: thread_id.to_string(),
                objective,
                status,
                token_budget,
                origin: Some(ThreadGoalMutationOrigin::User),
            },
        })
        .await?)
}

pub(crate) static TEST_MODEL_PRESETS: LazyLock<Vec<ModelPreset>> = LazyLock::new(|| {
    let mut response = bundled_models_response()
        .unwrap_or_else(|err| panic!("bundled models.json should parse: {err}"));
    response.models.sort_by_key(|model| model.priority);
    let mut presets: Vec<ModelPreset> = response.models.into_iter().map(Into::into).collect();
    ModelPreset::mark_default_by_picker_visibility(&mut presets);
    presets
});

#[cfg(unix)]
pub(crate) fn temp_runtime_dir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("tui-")
        .tempdir_in(codex_uds::prepare_shared_daemon_socket_directory()?)
}
pub(crate) fn test_path_display(path: &str) -> String {
    test_path_buf(path).display().to_string()
}

pub(crate) fn session_source_cli<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(codex_app_server_protocol::SessionSource::Cli)
}

pub(crate) fn skill_scope_user<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(codex_app_server_protocol::SkillScope::User)
}

pub(crate) fn skill_scope_repo<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(codex_app_server_protocol::SkillScope::Repo)
}

fn from_app_server_wire<T>(value: impl Serialize) -> T
where
    T: DeserializeOwned,
{
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .unwrap_or_else(|err| {
            panic!("app-server wire value should map to legacy helper type: {err}")
        })
}

use crate::app_event::PermissionProfileSelection;
use crate::app_server_session::session_preparation::RequestPolicy;
use crate::app_server_session::{
    AppServerSession, AppServerStartedThread, ForkGoalContinuation, ForkName,
    is_thread_settings_update_unsupported,
};
use crate::legacy_core::config::Config;
use crate::local_settings::LocalSettings;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::{
    ClientRequest, RequestId, ThreadInjectItemsParams, ThreadInjectItemsResponse,
    ThreadLoadedListParams, ThreadLoadedListResponse, ThreadNameMode, ThreadSetNameParams,
    ThreadSetNameResponse, ThreadSettingsUpdateParams, ThreadSettingsUpdateResponse,
    TurnInterruptParams, TurnInterruptResponse,
};
use codex_protocol::ThreadId;
use codex_protocol::models::ResponseItem;
use color_eyre::eyre::{Result, WrapErr};

impl AppServerSession {
    #[expect(
        clippy::too_many_arguments,
        reason = "fork position and named permissions are independent"
    )]
    pub(crate) async fn fork_thread_at(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        last_turn_id: Option<String>,
        before_turn_id: Option<String>,
        goal_continuation: ForkGoalContinuation,
        selected_profile: Option<&PermissionProfileSelection>,
        name: Option<ForkName>,
    ) -> Result<AppServerStartedThread> {
        let prepared = self
            .prepare_fork_thread(
                RequestPolicy::Unbounded,
                local_settings,
                config,
                thread_id,
                last_turn_id,
                before_turn_id,
                goal_continuation,
                selected_profile,
                name,
            )
            .run()
            .await;
        self.apply_session_observations(prepared.observations);
        prepared.result
    }

    pub(crate) async fn fork_side_thread(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        selected_profile: Option<&PermissionProfileSelection>,
    ) -> Result<AppServerStartedThread> {
        let prepared = self
            .prepare_side_thread(
                RequestPolicy::Unbounded,
                local_settings,
                config,
                thread_id,
                selected_profile,
                Vec::new(),
            )
            .run()
            .await;
        self.apply_session_observations(prepared.observations);
        prepared.result
    }

    pub(crate) async fn thread_loaded_list(
        &mut self,
        params: ThreadLoadedListParams,
    ) -> Result<ThreadLoadedListResponse> {
        let request_id = RequestId::String(uuid::Uuid::new_v4().to_string());
        self.request_handle()
            .request_typed(ClientRequest::ThreadLoadedList { request_id, params })
            .await
            .wrap_err("failed to list loaded threads from app server")
    }

    pub(crate) async fn thread_settings_update(
        &mut self,
        params: ThreadSettingsUpdateParams,
    ) -> Result<bool> {
        if !self.operation_settings_supported() {
            return Ok(false);
        }
        let request_id = RequestId::String(uuid::Uuid::new_v4().to_string());
        match self
            .request_handle()
            .request_typed::<ThreadSettingsUpdateResponse>(ClientRequest::ThreadSettingsUpdate {
                request_id,
                params,
            })
            .await
        {
            Ok(_) => Ok(true),
            Err(TypedRequestError::Server { source, .. })
                if is_thread_settings_update_unsupported(&source) =>
            {
                // Older remote app servers can reject this experimental method as
                // method-not-found, experimental-capability-gated, or an unknown
                // request variant. Treat those as a session-level capability
                // downgrade so local TUI setting changes stay best-effort instead
                // of showing an error every time the user changes model, effort, or mode.
                self.note_operation_settings_unsupported();
                Ok(false)
            }
            Err(err) => Err(err).wrap_err("thread/settings/update failed in TUI"),
        }
    }

    pub(crate) async fn thread_inject_items(
        &mut self,
        thread_id: ThreadId,
        items: Vec<ResponseItem>,
    ) -> Result<ThreadInjectItemsResponse> {
        let items = items
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("failed to encode thread/inject_items payload")?;
        let request_id = RequestId::String(uuid::Uuid::new_v4().to_string());
        self.request_handle()
            .request_typed(ClientRequest::ThreadInjectItems {
                request_id,
                params: ThreadInjectItemsParams {
                    thread_id: thread_id.to_string(),
                    items,
                },
            })
            .await
            .wrap_err("thread/inject_items failed during TUI side conversation setup")
    }

    pub(crate) async fn turn_interrupt(
        &mut self,
        thread_id: ThreadId,
        turn_id: String,
    ) -> std::result::Result<(), TypedRequestError> {
        let request_id = RequestId::String(uuid::Uuid::new_v4().to_string());
        let _: TurnInterruptResponse = self
            .request_handle()
            .request_typed(ClientRequest::TurnInterrupt {
                request_id,
                params: TurnInterruptParams {
                    thread_id: thread_id.to_string(),
                    turn_id,
                },
            })
            .await?;
        Ok(())
    }

    pub(crate) async fn startup_interrupt(
        &mut self,
        thread_id: ThreadId,
    ) -> std::result::Result<(), TypedRequestError> {
        self.turn_interrupt(thread_id, String::new()).await
    }

    pub(crate) async fn thread_set_name(
        &mut self,
        thread_id: ThreadId,
        name: String,
    ) -> Result<String> {
        self.thread_allocate_name(ThreadSetNameParams {
            thread_id: thread_id.to_string(),
            name,
            naming_mode: Some(ThreadNameMode::Explicit),
            conflict_thread_ids: None,
        })
        .await
    }

    pub(crate) async fn thread_allocate_name(
        &mut self,
        params: ThreadSetNameParams,
    ) -> Result<String> {
        let generated = params.naming_mode == Some(ThreadNameMode::Generated);
        let requested_name = params.name.clone();
        let request_id = RequestId::String(uuid::Uuid::new_v4().to_string());
        let response: ThreadSetNameResponse = self
            .request_handle()
            .request_typed(ClientRequest::ThreadSetName { request_id, params })
            .await?;
        match response.thread_name {
            Some(name) => Ok(name),
            None if generated => Err(color_eyre::eyre::eyre!(
                "The connected server does not return allocated names. Upgrade the server to enable automatic titles."
            )),
            None => Ok(requested_name),
        }
    }

    pub(crate) async fn thread_turns_page(
        &mut self,
        thread_id: ThreadId,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<codex_app_server_protocol::ThreadTurnsListResponse> {
        self.request_handle()
            .request_typed(ClientRequest::ThreadTurnsList {
                request_id: RequestId::String(uuid::Uuid::new_v4().to_string()),
                params: codex_app_server_protocol::ThreadTurnsListParams {
                    thread_id: thread_id.to_string(),
                    cursor,
                    limit: Some(limit),
                    sort_direction: Some(codex_app_server_protocol::SortDirection::Desc),
                    items_view: Some(codex_app_server_protocol::TurnItemsView::NotLoaded),
                },
            })
            .await
            .wrap_err("failed to load a bounded thread history page")
    }
}
