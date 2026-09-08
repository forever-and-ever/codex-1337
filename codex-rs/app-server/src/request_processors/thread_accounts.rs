use super::*;
use codex_app_server_protocol::SavedAccount;
use codex_app_server_protocol::ThreadAccountListParams;
use codex_app_server_protocol::ThreadAccountListResponse;
use codex_app_server_protocol::ThreadAccountSelectParams;
use codex_app_server_protocol::ThreadAccountSelectResponse;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;

impl TurnRequestProcessor {
    pub(crate) async fn thread_account_list(
        &self,
        params: ThreadAccountListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        let config = self
            .config_manager
            .load_non_project_config()
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        let accounts = self
            .auth_manager
            .saved_accounts()
            .map_err(|error| internal_error(error.to_string()))?
            .into_iter()
            .map(|account| SavedAccount {
                alias: config
                    .wrangler
                    .account_aliases
                    .iter()
                    .find_map(|(alias, id)| (id == &account.id).then(|| alias.clone())),
                id: account.id,
                email: account.email,
                plan: account.plan,
            })
            .collect();
        Ok(Some(
            ThreadAccountListResponse {
                accounts,
                selected_account_id: thread.thread_settings_snapshot().await.account_id,
            }
            .into(),
        ))
    }

    pub(crate) async fn thread_account_select(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadAccountSelectParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        self.ensure_direct_input_allowed(request_id, thread.as_ref())
            .await?;
        let config = self
            .config_manager
            .load_non_project_config()
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        let id = config
            .wrangler
            .account_aliases
            .get(&params.account_id)
            .cloned()
            .unwrap_or(params.account_id);
        thread
            .select_account(&id)
            .await
            .map_err(|error| invalid_request(error.to_string()))?;
        let default_saved = ConfigEditsBuilder::for_config(&config)
            .with_edits([ConfigEdit::SetPath {
                segments: vec!["wrangler".into(), "default_account".into()],
                value: toml_edit::value(&id),
            }])
            .apply()
            .await
            .map_err(|error| tracing::warn!(%error, "could not save default account"))
            .is_ok();
        Ok(Some(
            ThreadAccountSelectResponse {
                account_id: id,
                default_saved,
            }
            .into(),
        ))
    }
}
