//! Live terminal restoration runs through a request-only worker, never history replay.

use super::AppServerSession;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ThreadBackgroundTerminal;
use codex_app_server_protocol::ThreadBackgroundTerminalsListParams;
use codex_app_server_protocol::ThreadBackgroundTerminalsListResponse;
use codex_protocol::ThreadId;
use color_eyre::Result;
use std::collections::HashSet;

impl AppServerSession {
    pub(crate) async fn list_background_terminals(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<Vec<ThreadBackgroundTerminal>> {
        let mut terminals = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let request_id = self.next_request_id();
            let page: ThreadBackgroundTerminalsListResponse = self
                .request_handle
                .request_typed(ClientRequest::ThreadBackgroundTerminalsList {
                    request_id,
                    params: ThreadBackgroundTerminalsListParams {
                        thread_id: thread_id.to_string(),
                        scope: codex_app_server_protocol::ThreadBackgroundTerminalsScope::default(),
                        cursor,
                        limit: None,
                    },
                })
                .await?;
            terminals.extend(page.data);
            let Some(next) = page.next_cursor else {
                return Ok(terminals);
            };
            color_eyre::eyre::ensure!(
                seen.insert(next.clone()),
                "repeated background-terminal cursor"
            );
            cursor = Some(next);
        }
    }
}
