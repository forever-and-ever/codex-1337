use super::AppServerSession;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::CommsGate;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadMetadataUpdateParams;
use codex_app_server_protocol::ThreadMetadataUpdateResponse;
use codex_protocol::ThreadId;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;

impl AppServerSession {
    pub(crate) async fn thread_comms_update(
        &mut self,
        thread_id: ThreadId,
        comms: CommsGate,
    ) -> Result<Thread> {
        let request_id = self.next_request_id();
        let response: ThreadMetadataUpdateResponse = self
            .client
            .request_typed(ClientRequest::ThreadMetadataUpdate {
                request_id,
                params: ThreadMetadataUpdateParams {
                    thread_id: thread_id.to_string(),
                    comms: Some(comms),
                    project_id: None,
                    daybreak_enabled: None,
                    git_info: None,
                },
            })
            .await
            .wrap_err("thread/metadata/update failed")?;
        Ok(response.thread)
    }
}
