use codex_app_server_protocol::CommsGate;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadStatus;

use super::invalid_request;

pub(super) fn ensure_recipient(destination: &Thread) -> Result<(), JSONRPCErrorError> {
    match destination.comms {
        CommsGate::Open => Ok(()),
        CommsGate::Closed if matches!(destination.status, ThreadStatus::Active { .. }) => Ok(()),
        CommsGate::Closed => Err(invalid_request(
            "Message undeliverable: that context is currently closed to work and is inactive.",
        )),
        CommsGate::Blocked => Err(invalid_request(
            "Message undeliverable: that context is blocked and accepts no messages.",
        )),
    }
}
