use super::*;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ThreadAccountListParams;
use codex_app_server_protocol::ThreadAccountListResponse;
use codex_app_server_protocol::ThreadAccountSelectParams;
use codex_app_server_protocol::ThreadAccountSelectResponse;

#[derive(Default)]
pub(super) struct AccountUi {
    serial: u64,
    pub(super) displayed: Option<(ThreadId, codex_app_server_protocol::GetAccountResponse)>,
}

impl App {
    pub(super) fn refresh_conversation_account(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
    ) {
        self.account_ui.serial = self.account_ui.serial.wrapping_add(1);
        let serial = self.account_ui.serial;
        let handle = server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = handle
                .request_typed::<codex_app_server_protocol::GetAccountResponse>(
                    ClientRequest::GetAccount {
                        request_id: codex_app_server_protocol::RequestId::String(format!(
                            "thread-account-{}",
                            uuid::Uuid::new_v4()
                        )),
                        params: codex_app_server_protocol::GetAccountParams {
                            thread_id: Some(thread_id.to_string()),
                            refresh_token: false,
                        },
                    },
                )
                .await
                .map_err(|error| error.to_string());
            tx.send(AppEvent::ConversationAccountLoaded {
                thread_id,
                serial,
                result,
            });
        });
    }

    pub(super) fn apply_conversation_account(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        serial: u64,
        result: Result<codex_app_server_protocol::GetAccountResponse, String>,
    ) {
        if self.current_displayed_thread_id() != Some(thread_id) || self.account_ui.serial != serial
        {
            return;
        }
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, "cannot read conversation account");
                self.account_ui.displayed = None;
                self.chat_widget
                    .update_account_state(None, None, false, false);
                return;
            }
        };
        use crate::status::StatusAccountDisplay;
        use codex_app_server_protocol::Account;
        // Backend-only host authentication has no Account projection. Its
        // capability flag remains owned by the global auth notifications.
        let backend_only = response.account_id.is_none()
            && response.account.is_none()
            && self.chat_widget.has_codex_backend_auth()
            && !self.chat_widget.has_chatgpt_account();
        let (display, plan, chatgpt) = match response.account.clone() {
            Some(Account::Chatgpt { email, plan_type }) => (
                Some(StatusAccountDisplay::ChatGpt {
                    email,
                    plan: Some(crate::status::plan_type_display_name(plan_type)),
                }),
                Some(plan_type),
                true,
            ),
            Some(Account::ApiKey {}) => (Some(StatusAccountDisplay::ApiKey), None, false),
            Some(Account::AmazonBedrock { .. }) | None => (None, None, false),
        };
        if self.account_ui.displayed.as_ref() == Some(&(thread_id, response.clone()))
            && self.chat_widget.status_account_display() == display.as_ref()
        {
            return;
        }
        self.chat_widget.cyber_policy_notice = Default::default();
        if chatgpt {
            crate::daybreak::prefetch_notice(
                &self.config,
                server,
                self.chat_widget.cyber_policy_notice.clone(),
                response.account_id.clone(),
            );
        }
        self.account_ui.displayed = Some((thread_id, response));
        self.rate_limit_hard_stop_generation = self.rate_limit_hard_stop_generation.wrapping_add(1);
        self.rate_limit_refresh_state.invalidate_recovery();
        self.last_thread_usage_status_cell = None;
        self.pending_thread_usage_history_refresh = false;
        self.chat_widget
            .update_account_state(display, plan, chatgpt, chatgpt || backend_only);
        if chatgpt {
            let reset_hint_request_id = self.chat_widget.start_rate_limit_reset_startup_check();
            self.refresh_rate_limits(
                server,
                RateLimitRefreshOrigin::StartupPrefetch {
                    reset_hint_request_id,
                },
            );
        }
    }

    pub(super) async fn open_account_picker(
        &mut self,
        server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        let request_id = server.next_request_id();
        let result = server
            .request_handle()
            .request_typed::<ThreadAccountListResponse>(ClientRequest::ThreadAccountList {
                request_id,
                params: ThreadAccountListParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await;
        if self.current_displayed_thread_id() != Some(thread_id) {
            return;
        }
        match result {
            Ok(accounts) => self.chat_widget.show_account_picker(thread_id, accounts),
            Err(error) => self
                .chat_widget
                .add_error_message(format!("Cannot list accounts: {error}")),
        }
    }

    pub(super) async fn select_account(
        &mut self,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        account_id: String,
    ) {
        let request_id = server.next_request_id();
        let result = server
            .request_handle()
            .request_typed::<ThreadAccountSelectResponse>(ClientRequest::ThreadAccountSelect {
                request_id,
                params: ThreadAccountSelectParams {
                    thread_id: thread_id.to_string(),
                    account_id,
                },
            })
            .await;
        if self.current_displayed_thread_id() != Some(thread_id) {
            return;
        }
        match result {
            Ok(response) => {
                self.rate_limit_hard_stop_generation =
                    self.rate_limit_hard_stop_generation.wrapping_add(1);
                self.rate_limit_refresh_state.invalidate_recovery();
                self.last_thread_usage_status_cell = None;
                self.pending_thread_usage_history_refresh = false;
                self.refresh_conversation_account(server, thread_id);
                if response.default_saved {
                    self.config.wrangler.default_account = Some(response.account_id);
                }
                self.chat_widget.add_info_message(
                    "Conversation account changed.".to_owned(),
                    (!response.default_saved)
                        .then(|| "Could not save the default for new conversations.".to_owned()),
                );
            }
            Err(error) => self
                .chat_widget
                .add_error_message(format!("Cannot change account: {error}")),
        }
    }
}
