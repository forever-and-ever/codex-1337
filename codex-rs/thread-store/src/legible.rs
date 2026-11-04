//! Stored history written by builds of other ages.
//!
//! Every installed build reads one history store: a staging channel runs beside production,
//! and an installation can roll back. A stored item may therefore carry a kind or a shape the
//! reading build does not know. Readers omit such an item, as rollout replay and projection
//! skip a line they cannot parse, so no stored item can make a thread unreadable.

use codex_app_server_protocol::ThreadItemPayload;

/// Reads a stored [`ThreadItemPayload`], or omits it when this build cannot.
pub fn legible_item(item_id: &str, item_json: &[u8]) -> Option<ThreadItemPayload> {
    serde_json::from_slice(item_json)
        .inspect_err(|err| {
            tracing::debug!(
                item_id,
                "omitting a stored thread item this build cannot read: {err}"
            );
        })
        .ok()
}
