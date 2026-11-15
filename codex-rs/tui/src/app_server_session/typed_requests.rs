//! Typed RPC counterparts of the app-server session methods used by UI continuations.
//!
//! Request IDs remain caller-owned: operation and verification lanes attach their own identity.

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol as api;
use std::future::Future;

macro_rules! typed_requests {
    ($($(#[$attribute:meta])* $method:ident($params:ty) -> $response:ty = $variant:ident;)*) => {
        pub(crate) trait SessionRpc {
            $($(#[$attribute])* fn $method(&self, request_id: api::RequestId, params: $params)
                -> impl Future<Output = Result<$response, TypedRequestError>> + Send;)*

            fn thread_settings_update(&self, request_id: api::RequestId, params: api::ThreadSettingsUpdateParams)
                -> impl Future<Output = Result<bool, TypedRequestError>> + Send;

            fn thread_metadata_update_branch(&self, request_id: api::RequestId, params: api::ThreadMetadataUpdateParams)
                -> impl Future<Output = Result<api::ThreadMetadataUpdateResponse, TypedRequestError>> + Send;
        }

        impl SessionRpc for AppServerRequestHandle {
            $($(#[$attribute])* async fn $method(&self, request_id: api::RequestId, params: $params)
                -> Result<$response, TypedRequestError> {
                self.request_typed(api::ClientRequest::$variant { request_id, params }).await
            })*

            async fn thread_settings_update(&self, request_id: api::RequestId, params: api::ThreadSettingsUpdateParams)
                -> Result<bool, TypedRequestError> {
                match self.request_typed::<api::ThreadSettingsUpdateResponse>(api::ClientRequest::ThreadSettingsUpdate { request_id, params }).await {
                    Ok(_) => Ok(true),
                    Err(TypedRequestError::Server { ref source, .. }) if super::is_thread_settings_update_unsupported(source) => Ok(false),
                    Err(error) => Err(error),
                }
            }

            async fn thread_metadata_update_branch(&self, request_id: api::RequestId, params: api::ThreadMetadataUpdateParams)
                -> Result<api::ThreadMetadataUpdateResponse, TypedRequestError> {
                self.thread_metadata_update(request_id, params).await
            }
        }
    };
}

typed_requests! {
    thread_prepare_approved_plan(api::ThreadPrepareApprovedPlanParams) -> api::ThreadPrepareApprovedPlanResponse = ThreadPrepareApprovedPlan;
    thread_read(api::ThreadReadParams) -> api::ThreadReadResponse = ThreadRead;
    thread_loaded_list(api::ThreadLoadedListParams) -> api::ThreadLoadedListResponse = ThreadLoadedList;
    thread_allocate_name(api::ThreadSetNameParams) -> api::ThreadSetNameResponse = ThreadSetName;
    thread_metadata_update(api::ThreadMetadataUpdateParams) -> api::ThreadMetadataUpdateResponse = ThreadMetadataUpdate;
    thread_memory_mode_set(api::ThreadMemoryModeSetParams) -> api::ThreadMemoryModeSetResponse = ThreadMemoryModeSet;
    memory_reset(Option<()>) -> api::MemoryResetResponse = MemoryReset;
    thread_goal_get(api::ThreadGoalGetParams) -> api::ThreadGoalGetResponse = ThreadGoalGet;
    thread_goal_set(api::ThreadGoalSetParams) -> api::ThreadGoalSetResponse = ThreadGoalSet;
    thread_goal_clear(api::ThreadGoalClearParams) -> api::ThreadGoalClearResponse = ThreadGoalClear;
    thread_goal_boxes_read(api::ThreadGoalBoxesReadParams) -> api::ThreadGoalBoxesReadResponse = ThreadGoalBoxesRead;
    thread_goal_timebox_set(api::ThreadGoalTimeboxSetParams) -> api::ThreadGoalTimeboxSetResponse = ThreadGoalTimeboxSet;
    thread_goal_quotabox_set(api::ThreadGoalQuotaboxSetParams) -> api::ThreadGoalQuotaboxSetResponse = ThreadGoalQuotaboxSet;
    turn_interrupt(api::TurnInterruptParams) -> api::TurnInterruptResponse = TurnInterrupt;
    #[cfg(test)]
    thread_inject_items(api::ThreadInjectItemsParams) -> api::ThreadInjectItemsResponse = ThreadInjectItems;
    logout_account(api::NullableAccountScopeParams) -> api::LogoutAccountResponse = LogoutAccount;
}
