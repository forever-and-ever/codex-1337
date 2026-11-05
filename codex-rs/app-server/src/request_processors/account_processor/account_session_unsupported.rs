//! The retained account-session wire API cannot change a managed login's workspace.

use codex_app_server_protocol::JSONRPCErrorError;
use serde::Serialize;

#[derive(Serialize)]
#[serde(tag = "reason", rename_all = "camelCase")]
enum Unsupported {
    WorkspaceSwitchingUnsupported,
}

pub(crate) fn error() -> JSONRPCErrorError {
    JSONRPCErrorError {
        code: crate::error_code::INVALID_REQUEST_ERROR_CODE,
        message: "Workspace switching through accountSession is unsupported. Select a saved account instead; in the TUI, use /account.".into(),
        data: Some(serde_json::json!(Unsupported::WorkspaceSwitchingUnsupported)),
    }
}
