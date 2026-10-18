//! Typed effective-configuration reads for UI continuations.

use super::AppServerRequestHandle;
use super::ClientRequest;
use super::ConfigReadParams;
use super::ConfigReadResponse;
use super::RequestId;
use super::TypedRequestError;
use super::Uuid;

pub(crate) async fn read_effective_config_typed(
    request_handle: AppServerRequestHandle,
    cwd: String,
) -> Result<ConfigReadResponse, TypedRequestError> {
    request_handle
        .request_typed(ClientRequest::ConfigRead {
            request_id: RequestId::String(format!("tui-config-read-{}", Uuid::new_v4())),
            params: ConfigReadParams {
                include_layers: false,
                cwd: Some(cwd),
            },
        })
        .await
}
