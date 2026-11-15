//! Account requests preview the fresh-root selection only until a conversation owns their scope.

use super::Account;
use super::AppServerSession;
use super::ClientRequest;
use super::Config;
use super::GetAccountParams;
use super::GetAccountResponse;
use super::Result;
use super::StatusAccountDisplay;
use super::TelemetryAuthMode;
use super::ThreadId;
#[cfg(test)]
use super::ThreadSessionState;
use super::bootstrap_request_error;
use super::plan_type_display_name;
use super::thread_start_params_from_config;
use super::workspace_display;
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

    pub(crate) fn draft_account_params(&self, config: &Config) -> NewThreadAccountParams {
        let mut params = self.new_thread_account_params(config);
        params.model_provider = Some(config.model_provider_id.clone());
        params
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
        self.request_handle
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

#[derive(Clone, Default)]
pub(crate) struct SessionAccount {
    pub(crate) display: Option<StatusAccountDisplay>,
    pub(crate) plan: Option<codex_protocol::account::PlanType>,
    pub(crate) requires_openai_auth: bool,
}

impl From<GetAccountResponse> for SessionAccount {
    fn from(response: GetAccountResponse) -> Self {
        let mut account = Self {
            requires_openai_auth: response.requires_openai_auth,
            ..Self::default()
        };
        account.display = match response.account {
            Some(Account::Chatgpt {
                email,
                plan_type,
                workspace_name,
            }) => {
                account.plan = Some(plan_type);
                Some(StatusAccountDisplay::ChatGpt {
                    email,
                    plan: Some(plan_type_display_name(plan_type)),
                    workspace: workspace_display(workspace_name, response.workspace_routing),
                })
            }
            Some(Account::ApiKey {}) => Some(StatusAccountDisplay::ApiKey),
            Some(Account::ClaudeSubscription { email }) => {
                Some(StatusAccountDisplay::ClaudeSubscription { email })
            }
            None => None,
        };
        account
    }
}

impl SessionAccount {
    pub(crate) fn has_chatgpt_account(&self) -> bool {
        matches!(self.display, Some(StatusAccountDisplay::ChatGpt { .. }))
    }

    pub(crate) fn auth_mode(&self) -> Option<TelemetryAuthMode> {
        match self.display {
            Some(StatusAccountDisplay::ChatGpt { .. }) => Some(TelemetryAuthMode::Chatgpt),
            Some(StatusAccountDisplay::ApiKey) => Some(TelemetryAuthMode::ApiKey),
            Some(StatusAccountDisplay::ClaudeSubscription { .. }) | None => None,
        }
    }

    pub(crate) fn email(&self) -> Option<&str> {
        match &self.display {
            Some(StatusAccountDisplay::ChatGpt { email, .. })
            | Some(StatusAccountDisplay::ClaudeSubscription { email }) => email.as_deref(),
            Some(StatusAccountDisplay::ApiKey) | None => None,
        }
    }
}

impl AppServerSession {
    #[cfg(test)]
    pub(crate) async fn read_thread_account(
        &mut self,
        session: &ThreadSessionState,
    ) -> Result<SessionAccount> {
        // TELOS(pinned-accounts): the thread owns its payer, not the server's current default.
        let request_id = self.next_request_id();
        let response: GetAccountResponse = self
            .request_handle
            .request_typed(ClientRequest::GetAccount {
                request_id,
                params: GetAccountParams {
                    thread_id: Some(session.thread_id.to_string()),
                    new_thread: None,
                    refresh_token: false,
                },
            })
            .await?;
        Ok(response.into())
    }
}
