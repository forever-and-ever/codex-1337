//! Draft identity is the server's fresh-root selection, not its global login.

use super::*;
use codex_app_server_protocol::NewThreadAccountParams;
use codex_utils_path_uri::LegacyAppPathString;

impl AppServerSession {
    pub(super) async fn read_new_thread_account(
        &mut self,
        config: &Config,
    ) -> Result<GetAccountResponse> {
        let params = thread_start_params_from_config(
            config,
            self.thread_params_mode(),
            self.remote_cwd_override.as_deref(),
            /*session_start_source*/ None,
        );
        let request_id = self.next_request_id();
        self.client
            .request_typed(ClientRequest::GetAccount {
                request_id,
                params: GetAccountParams {
                    thread_id: None,
                    new_thread: Some(NewThreadAccountParams {
                        model_provider: self.explicit_model_provider(config),
                        cwd: params.cwd.map(LegacyAppPathString::from_string),
                        config: params.config,
                    }),
                    refresh_token: false,
                },
            })
            .await
            .map_err(|error| {
                bootstrap_request_error("account/read failed during TUI bootstrap", error)
            })
    }
}
