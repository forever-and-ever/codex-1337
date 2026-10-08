use super::*;
use codex_app_server_protocol::ContextRole;
use codex_app_server_protocol::ThreadSetContextRoleParams;
use codex_app_server_protocol::ThreadSetContextRoleResponse;

impl ThreadRequestProcessor {
    pub(crate) async fn thread_set_context_role(
        &self,
        params: ThreadSetContextRoleParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let role = params
            .context_role
            .as_deref()
            .map(ContextRole::new)
            .transpose()
            .map_err(invalid_request)?;
        let _permit = self.acquire_thread_list_state_permit().await?;
        if let Ok(thread) = self.thread_manager.get_thread(thread_id).await {
            if thread.config_snapshot().await.ephemeral {
                return Err(invalid_request(
                    "ephemeral conversations cannot persist a context role",
                ));
            }
            self.thread_store
                .persist_thread(thread_id, PersistContext::Standard)
                .await
                .map_err(|err| thread_store_mutation_error("persist context role target", err))?;
        }
        let updated = self
            .thread_manager
            .update_thread_metadata(
                thread_id,
                StoreThreadMetadataPatch {
                    context_role: Some(role),
                    ..Default::default()
                },
                /*include_archived*/ false,
            )
            .await
            .map_err(|err| core_thread_write_error("set context role", err))?;
        Ok(Some(
            ThreadSetContextRoleResponse {
                context_role: updated.context_role.map(|role| role.as_str().to_owned()),
            }
            .into(),
        ))
    }
}
