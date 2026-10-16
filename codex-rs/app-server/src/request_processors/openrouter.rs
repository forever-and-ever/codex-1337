use crate::error_code::invalid_request;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;

pub(crate) fn unavailable() -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
    Err(invalid_request(
        "OpenRouter requests are not implemented yet",
    ))
}
