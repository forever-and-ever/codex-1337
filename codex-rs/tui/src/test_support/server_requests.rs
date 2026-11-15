//! Test-only session requests outside the upstream support module.

use crate::app_server_session::typed_requests::SessionRpc;

pub(crate) async fn thread_goal_get(
    server: &AppServerSession,
    thread_id: ThreadId,
) -> Result<codex_app_server_protocol::ThreadGoalGetResponse> {
    use codex_app_server_protocol::RequestId;
    use codex_app_server_protocol::ThreadGoalGetParams;
    Ok(server
        .request_handle()
        .thread_goal_get(
            RequestId::String(format!("test-goal-read-{}", uuid::Uuid::new_v4())),
            ThreadGoalGetParams {
                thread_id: thread_id.to_string(),
            },
        )
        .await?)
}

#[expect(
    clippy::option_option,
    reason = "Wire patch distinguishes keeping, clearing, and setting the token budget"
)]
pub(crate) async fn thread_goal_set(
    server: &AppServerSession,
    thread_id: ThreadId,
    objective: Option<String>,
    status: Option<codex_app_server_protocol::ThreadGoalStatus>,
    token_budget: Option<Option<i64>>,
) -> Result<codex_app_server_protocol::ThreadGoalSetResponse> {
    use codex_app_server_protocol::RequestId;
    use codex_app_server_protocol::ThreadGoalMutationOrigin;
    use codex_app_server_protocol::ThreadGoalSetParams;
    Ok(server
        .request_handle()
        .thread_goal_set(
            RequestId::String(format!("test-goal-set-{}", uuid::Uuid::new_v4())),
            ThreadGoalSetParams {
                thread_id: thread_id.to_string(),
                objective,
                status,
                token_budget,
                origin: Some(ThreadGoalMutationOrigin::User),
            },
        )
        .await?)
}

use crate::app_event::PermissionProfileSelection;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::AppServerStartedThread;
use crate::app_server_session::ForkGoalContinuation;
use crate::app_server_session::ForkName;
use crate::app_server_session::is_thread_settings_update_unsupported;
use crate::app_server_session::session_preparation::RequestPolicy;
use crate::legacy_core::config::Config;
use crate::local_settings::LocalSettings;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadInjectItemsParams;
use codex_app_server_protocol::ThreadInjectItemsResponse;
use codex_app_server_protocol::ThreadSettingsUpdateParams;
use codex_app_server_protocol::ThreadSettingsUpdateResponse;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_protocol::ThreadId;
use codex_protocol::models::ResponseItem;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;

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
        let prepared = Box::pin(
            self.prepare_fork_thread(
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
            .run(),
        )
        .await;
        self.apply_session_observations(prepared.observations)?;
        prepared.result
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
            .thread_inject_items(
                request_id,
                ThreadInjectItemsParams {
                    thread_id: thread_id.to_string(),
                    items,
                },
            )
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
            .turn_interrupt(
                request_id,
                TurnInterruptParams {
                    thread_id: thread_id.to_string(),
                    turn_id,
                },
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn startup_interrupt(
        &mut self,
        thread_id: ThreadId,
    ) -> std::result::Result<(), TypedRequestError> {
        self.turn_interrupt(thread_id, String::new()).await
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
