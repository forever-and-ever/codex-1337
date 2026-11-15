//! App-server signaling for TUI-owned realtime WebRTC sessions.

use super::AppServerSession;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ThreadRealtimeStopParams;
use codex_app_server_protocol::ThreadRealtimeStopResponse;
use codex_protocol::ThreadId;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;

impl AppServerSession {
    pub(crate) async fn thread_realtime_stop(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadRealtimeStopResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadRealtimeStop {
                request_id,
                params: ThreadRealtimeStopParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/realtime/stop failed in TUI")?;
        Ok(())
    }
}
