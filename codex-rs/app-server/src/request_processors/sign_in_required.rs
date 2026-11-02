use codex_app_server_protocol::CodexErrorInfo;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_protocol::sign_in_required::SignInRequired;

/// TELOS(recoverable-sign-in): request errors carry the same classification as turn errors.
pub(super) fn sign_in_required(required: &SignInRequired) -> JSONRPCErrorError {
    JSONRPCErrorError {
        code: -32600,
        message: required.to_string(),
        data: Some(serde_json::json!({
            "codexErrorInfo": CodexErrorInfo::SignInRequired(required.clone()),
        })),
    }
}
