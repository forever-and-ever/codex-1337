//! Account requests preview the fresh-root selection only until a conversation owns their scope.

use super::AppServerSession;
use super::ClientRequest;
use super::Config;
use super::GetAccountParams;
use super::GetAccountResponse;
use super::Result;
use super::ThreadId;
use super::bootstrap_request_error;
use super::thread_start_params_from_config;
use codex_app_server_protocol::NewThreadAccountParams;
use codex_utils_path_uri::LegacyAppPathString;

impl AppServerSession {
    pub(crate) fn new_thread_account_params(&self, config: &Config) -> NewThreadAccountParams {
        let params = thread_start_params_from_config(
            config,
            self.thread_params_mode(),
            self.remote_cwd_override.as_deref(),
            /*session_start_source*/ None,
        );
        NewThreadAccountParams {
            model_provider: self.explicit_model_provider(config),
            cwd: params.cwd.map(LegacyAppPathString::from_string),
            config: params.config,
        }
    }

    pub(super) async fn read_bootstrap_account(
        &mut self,
        config: &Config,
        thread_id: Option<ThreadId>,
    ) -> Result<GetAccountResponse> {
        let new_thread = thread_id
            .is_none()
            .then(|| self.new_thread_account_params(config));
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::GetAccount {
                request_id,
                params: GetAccountParams {
                    thread_id: thread_id.map(|id| id.to_string()),
                    new_thread,
                    refresh_token: false,
                },
            })
            .await
            .map_err(|error| {
                bootstrap_request_error("account/read failed during TUI bootstrap", error)
            })
    }
}
