use super::AppServerSession;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ContextRole;
use codex_app_server_protocol::ThreadSetContextRoleParams;
use codex_app_server_protocol::ThreadSetContextRoleResponse;
use codex_protocol::ThreadId;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;

impl AppServerSession {
    pub(crate) async fn thread_set_context_role(
        &mut self,
        thread_id: ThreadId,
        role: Option<&ContextRole>,
    ) -> Result<Option<String>> {
        let request_id = self.next_request_id();
        let response: ThreadSetContextRoleResponse = self
            .client
            .request_typed(ClientRequest::ThreadSetContextRole {
                request_id,
                params: ThreadSetContextRoleParams {
                    thread_id: thread_id.to_string(),
                    context_role: role.map(|role| role.as_str().to_owned()),
                },
            })
            .await
            .wrap_err("thread/contextRole/set failed")?;
        Ok(response.context_role)
    }
}
