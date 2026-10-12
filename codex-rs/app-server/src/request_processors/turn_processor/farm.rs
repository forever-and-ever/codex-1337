use super::TurnRequestProcessor;
use super::validate_user_input_image_urls;
use crate::error_code::internal_error;
use crate::error_code::invalid_params;
use crate::outgoing_message::ConnectionRequestId;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadFarmMode;
use codex_app_server_protocol::ThreadFarmParams;
use codex_app_server_protocol::ThreadFarmResponse;
use codex_core::FarmError;
use codex_core::FarmMode;
use codex_core::FarmRequest;
use codex_protocol::error::CodexErrorDetails;

impl TurnRequestProcessor {
    pub(crate) async fn thread_farm(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadFarmParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let reject = |mut error: JSONRPCErrorError| {
            error.data = Some(
                serde_json::json!({ "farmOutcome": "rejected", "requestId": &params.request_id }),
            );
            error
        };
        validate_user_input_image_urls(&params.input).map_err(reject)?;
        Self::validate_v2_input_limit(&params.input).map_err(reject)?;
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        self.ensure_direct_input_allowed(request_id, &thread)
            .await
            .map_err(reject)?;
        let farm_request_id = params.request_id.clone();
        let receipt = thread.farm(FarmRequest {
            request_id: params.request_id,
            mode: match params.mode { ThreadFarmMode::Automatic => FarmMode::Automatic, ThreadFarmMode::FullHistory => FarmMode::FullHistory, ThreadFarmMode::Fresh => FarmMode::Fresh },
            input: params.input.into_iter().map(codex_app_server_protocol::UserInput::into_core).collect(),
            task_name: params.task_name,
        }).await.map_err(|error| match error {
            FarmError::Rejected(error) => {
                let mut error = if matches!(error.details(), CodexErrorDetails::InvalidRequest(_)) {
                    invalid_params(error.to_string())
                } else { internal_error(error.to_string()) };
                error.data = Some(serde_json::json!({ "farmOutcome": "rejected", "requestId": farm_request_id }));
                error
            },
            FarmError::IdentityMismatch { request_id, child } => {
                let mut error = invalid_params("requestId is already bound to a different farm submission; inspect the original delegation rather than resubmitting it");
                error.data = Some(serde_json::json!({
                    "farmOutcome": "identityMismatch", "requestId": request_id,
                    "childThreadId": child.as_ref().map(|child| child.child_thread_id.to_string()),
                    "agentPath": child.map(|child| child.agent_path),
                }));
                error
            },
            FarmError::Uncertain { request_id, child, message } => {
                let mut error = internal_error(message);
                error.data = Some(serde_json::json!({
                    "farmOutcome": "uncertain",
                    "requestId": request_id,
                    "childThreadId": child.as_ref().map(|child| child.child_thread_id.to_string()),
                    "agentPath": child.map(|child| child.agent_path),
                }));
                error
            }
        })?;
        Ok(Some(
            ThreadFarmResponse {
                child_thread_id: receipt.child_thread_id.to_string(),
                agent_path: receipt.agent_path,
            }
            .into(),
        ))
    }
}
