use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::Duration;
use super::ExitMode;
use super::HashMap;
use super::RateLimitRefreshOrigin;
use super::Result;
use super::ThreadId;
use super::Uuid;
use super::VecDeque;
use crate::app_server_session::typed_requests::SessionRpc;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::AccountNicknameSetParams;
use codex_app_server_protocol::AccountNicknameSetResponse;
use codex_app_server_protocol::AccountProvider;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::LoginAccountResponse;
use codex_app_server_protocol::ThreadAccountListParams;
use codex_app_server_protocol::ThreadAccountListResponse;
use codex_app_server_protocol::ThreadAccountSelectParams;
use codex_app_server_protocol::ThreadAccountSelectResponse;

#[derive(Default)]
pub(super) struct AccountUi {
    pub(in crate::app) serial: u64,
    // A newer identity read supersedes the data, not the submitted command.
    usage: Option<(Option<ThreadId>, String)>,
    picker_request: Option<Uuid>,
    pub(super) selection_request: Option<ThreadId>,
    login_request: Option<ClaudeLoginStartId>,
    claude_login: Option<String>,
    early_login_completions: VecDeque<codex_app_server_protocol::AccountLoginCompletedNotification>,
    nickname_request: Option<ThreadId>,
    pending: HashMap<ThreadId, String>,
    pub(super) displayed: Option<(
        Option<ThreadId>,
        codex_app_server_protocol::GetAccountResponse,
    )>,
}

impl App {
    pub(super) fn request_logout(&mut self, server: &AppServerSession) {
        use super::turn_submissions::ui_requests::UiRequestKind;
        use super::turn_submissions::ui_requests::UiRequestScope;
        let thread_id = self.current_displayed_thread_id();
        self.request_server(
            server,
            UiRequestScope::Connection,
            UiRequestKind::Mutation,
            "Logging out",
            move |handle| async move {
                handle
                    .logout_account(
                        codex_app_server_protocol::RequestId::String(Uuid::new_v4().to_string()),
                        thread_id.map(|id| codex_app_server_protocol::AccountScopeParams {
                            thread_id: Some(id.to_string()),
                        }),
                    )
                    .await
            },
            |app, _, result| {
                match result {
                    Ok(_) => app
                        .app_event_tx
                        .send(AppEvent::Exit(ExitMode::ShutdownFirst)),
                    Err(error) => {
                        tracing::error!(%error, "failed to logout");
                        app.chat_widget
                            .add_error_message(format!("Logout failed: {error}"));
                    }
                }
                Ok(())
            },
        );
    }

    pub(super) fn cancel_claude_login(&mut self, server: &mut AppServerSession, login_id: String) {
        if self.account_ui.claude_login.as_ref() != Some(&login_id) {
            return;
        }
        let handle = server.request_handle();
        let request_id = server.next_request_id();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = handle
                .request_typed::<codex_app_server_protocol::CancelLoginAccountResponse>(
                    ClientRequest::CancelLoginAccount {
                        request_id,
                        params: codex_app_server_protocol::CancelLoginAccountParams { login_id },
                    },
                )
                .await;
            if let Err(error) = result {
                tx.send(AppEvent::ClaudeLoginCancelFailed {
                    error: format!("Cannot cancel Claude sign-in: {error}"),
                });
            }
        });
    }

    pub(super) fn finish_claude_login(
        &mut self,
        notification: &codex_app_server_protocol::AccountLoginCompletedNotification,
    ) {
        // The response reader can receive completion before the start waiter runs.
        if self.account_ui.login_request.is_some() && self.account_ui.claude_login.is_none() {
            if self.account_ui.early_login_completions.len() == 2 {
                self.account_ui.early_login_completions.pop_front();
            }
            self.account_ui
                .early_login_completions
                .push_back(notification.clone());
            return;
        }
        if self.account_ui.claude_login.as_ref() != notification.login_id.as_ref()
            || notification.login_id.is_none()
        {
            return;
        }
        self.account_ui.claude_login = None;
        self.chat_widget.finish_claude_login_pending();
        if notification.success {
            self.app_event_tx.send(AppEvent::SignInRecovery(
                super::sign_in_required::RecoveryEvent::Retry(AccountProvider::ClaudeSubscription),
            ));
            self.chat_widget.add_info_message(
                "Claude account added. Reopen /account to select or rename it.".to_owned(),
                /*hint*/ None,
            );
        }
        if let Some(error) = &notification.error {
            self.chat_widget.add_error_message(error.clone());
        }
    }

    pub(super) fn set_account_nickname(
        &mut self,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        provider: AccountProvider,
        account_id: String,
        nickname: String,
    ) {
        if self.current_displayed_thread_id() != Some(thread_id)
            || self.account_ui.nickname_request.is_some()
        {
            return;
        }
        self.account_ui.nickname_request = Some(thread_id);
        let request_id = server.next_request_id();
        let handle = server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(/*secs*/ 30),
                handle.request_typed::<AccountNicknameSetResponse>(
                    ClientRequest::AccountNicknameSet {
                        request_id,
                        params: AccountNicknameSetParams {
                            provider,
                            account_id,
                            nickname,
                        },
                    },
                ),
            )
            .await
            .map_err(|_| "Rename not confirmed; reopen /account to check".to_owned())
            .and_then(|result| result.map_err(|error| error.to_string()));
            tx.send(AppEvent::AccountNicknameSetFinished { thread_id, result });
        });
    }

    pub(super) fn finish_account_nickname_set(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        result: Result<AccountNicknameSetResponse, String>,
    ) {
        if self.account_ui.nickname_request != Some(thread_id) {
            return;
        }
        self.account_ui.nickname_request = None;
        if self.current_displayed_thread_id() != Some(thread_id) {
            return;
        }
        match result {
            Ok(response) => {
                self.chat_widget.add_info_message(
                    if response.nickname.is_some() {
                        "Account nickname saved."
                    } else {
                        "Account nickname cleared."
                    }
                    .to_owned(),
                    /*hint*/ None,
                );
                self.refresh_conversation_account(server, thread_id);
            }
            Err(error) => self
                .chat_widget
                .add_error_message(format!("Cannot rename account: {error}")),
        }
    }

    pub(super) fn refresh_conversation_account(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
    ) {
        if self
            .account_ui
            .displayed
            .as_ref()
            .is_some_and(|(shown, _)| *shown != Some(thread_id))
        {
            self.chat_widget.active_account_label = None;
            self.chat_widget.refresh_status_line();
        }
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
                            Uuid::new_v4()
                        )),
                        params: codex_app_server_protocol::GetAccountParams {
                            new_thread: None,
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
        self.apply_displayed_account(server, Some(thread_id), serial, result);
    }

    pub(super) fn read_displayed_account(
        &mut self,
        server: &AppServerSession,
        usage: Option<String>,
    ) {
        use super::turn_submissions::ui_requests::UiRequestKind;
        use super::turn_submissions::ui_requests::UiRequestScope;
        self.account_ui.serial = self.account_ui.serial.wrapping_add(1);
        let serial = self.account_ui.serial;
        let thread_id = self.current_displayed_thread_id();
        if let Some(args) = usage {
            self.account_ui.usage = Some((thread_id, args));
        }
        let new_thread = thread_id
            .is_none()
            .then(|| server.draft_account_params(self.chat_widget.config_ref()));
        self.request_server(
            server,
            thread_id.map_or(UiRequestScope::Connection, UiRequestScope::DisplayedThread),
            UiRequestKind::ReadOnly,
            "Reading account",
            move |handle| async move {
                handle
                    .request_typed::<codex_app_server_protocol::GetAccountResponse>(
                        ClientRequest::GetAccount {
                            request_id: codex_app_server_protocol::RequestId::String(
                                Uuid::new_v4().to_string(),
                            ),
                            params: codex_app_server_protocol::GetAccountParams {
                                new_thread,
                                thread_id: thread_id.map(|id| id.to_string()),
                                refresh_token: false,
                            },
                        },
                    )
                    .await
            },
            move |app, server, result| {
                app.apply_displayed_account(
                    server,
                    thread_id,
                    serial,
                    result.map_err(ToString::to_string),
                );
                Ok(())
            },
        );
    }

    pub(super) fn apply_displayed_account(
        &mut self,
        server: &AppServerSession,
        thread_id: Option<ThreadId>,
        serial: u64,
        result: Result<codex_app_server_protocol::GetAccountResponse, String>,
    ) {
        if self.current_displayed_thread_id() != thread_id || self.account_ui.serial != serial {
            return;
        }
        let usage = self
            .account_ui
            .usage
            .take()
            .and_then(|(scope, args)| (scope == thread_id).then_some(args));
        match result {
            Ok(response) => {
                self.update_displayed_account(server, thread_id, response);
                if self.chat_widget.has_codex_backend_auth()
                    && self.chat_widget.has_chatgpt_account()
                    && self.chat_widget.account_ready_reason().is_none()
                {
                    crate::security_setup::prefetch(
                        &self.config,
                        server,
                        self.app_event_tx.clone(),
                        self.chat_widget.security_setup_request_id,
                    );
                }
                if let Some(args) = usage {
                    self.chat_widget.dispatch_usage(args);
                }
            }
            Err(error) if usage.is_some() => self
                .chat_widget
                .add_error_message(format!("Cannot read account for /usage: {error}")),
            Err(error) => tracing::warn!(%error, "cannot read account"),
        }
    }

    pub(super) fn update_displayed_account(
        &mut self,
        server: &AppServerSession,
        thread_id: Option<ThreadId>,
        response: codex_app_server_protocol::GetAccountResponse,
    ) {
        if let Some(thread_id) = thread_id
            && self.account_ui.pending.get(&thread_id) == response.account_id.as_ref()
            && self.account_ui.pending.remove(&thread_id).is_some()
        {
            self.chat_widget.add_info_message(
                "Conversation account changed.".to_owned(),
                /*hint*/ None,
            );
        }

        let session_account = crate::app_server_session::SessionAccount::from(response.clone());
        let display = session_account.display.clone();
        let chatgpt = session_account.has_chatgpt_account();
        let account_label = response
            .display_name
            .clone()
            .or_else(|| match &response.account {
                Some(Account::Chatgpt { email, .. } | Account::ClaudeSubscription { email }) => {
                    email.clone()
                }
                Some(Account::ApiKey {}) | None => None,
            })
            .or_else(|| response.account_id.clone());
        let account_label = account_label.and_then(|label| {
            let safe = label
                .chars()
                .filter(|character| {
                    !character.is_control()
                        && !matches!(*character as u32, 0x202A..=0x202E | 0x2066..=0x2069)
                })
                .take(96)
                .collect::<String>();
            (!safe.is_empty()).then_some(safe)
        });
        let active_account_label = self.chat_widget.openrouter_account_label().or(
            match (&response.account, account_label) {
                (Some(Account::Chatgpt { .. }), Some(label)) => Some(format!("ChatGPT · {label}")),
                (Some(Account::ClaudeSubscription { .. }), Some(label)) => {
                    Some(format!("Claude · {label}"))
                }
                _ => None,
            },
        );
        if self.chat_widget.account_ready_reason().is_none()
            && self.chat_widget.has_chatgpt_account() == chatgpt
            && self
                .account_ui
                .displayed
                .as_ref()
                .is_some_and(|(shown, account)| *shown == thread_id && account == &response)
            && self.chat_widget.status_account_display() == display.as_ref()
            && self.chat_widget.active_account_label == active_account_label
        {
            return;
        }
        if self.chat_widget.account_ready_reason().is_none()
            && self.chat_widget.has_chatgpt_account() == chatgpt
            && self
                .account_ui
                .displayed
                .as_ref()
                .is_some_and(|(shown, previous)| {
                    *shown == thread_id
                        && previous.account_id == response.account_id
                        && previous.requires_openai_auth == response.requires_openai_auth
                        && previous.workspace_routing == response.workspace_routing
                        && match (&previous.account, &response.account) {
                            (
                                Some(Account::Chatgpt {
                                    plan_type: previous,
                                    ..
                                }),
                                Some(Account::Chatgpt { plan_type, .. }),
                            ) => previous == plan_type,
                            (
                                Some(Account::ClaudeSubscription { .. }),
                                Some(Account::ClaudeSubscription { .. }),
                            )
                            | (Some(Account::ApiKey {}), Some(Account::ApiKey {}))
                            | (None, None) => true,
                            _ => false,
                        }
                })
        {
            self.account_ui.displayed = Some((thread_id, response));
            self.chat_widget.active_account_label = active_account_label;
            self.chat_widget.set_status_account_display(display);
            self.chat_widget.refresh_status_line();
            return;
        }
        self.account_ui.displayed = Some((thread_id, response));
        self.chat_widget.active_account_label = active_account_label;
        self.rate_limit_hard_stop_generation = self.rate_limit_hard_stop_generation.wrapping_add(1);
        self.rate_limit_refresh_state.invalidate_recovery();
        self.last_thread_usage_status_cell = None;
        self.pending_thread_usage_history_refresh = false;
        self.chat_widget.update_account_state(session_account);
        self.chat_widget.refresh_status_line();
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

    pub(super) fn open_account_picker(
        &mut self,
        server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        if let Some(login_id) = self.account_ui.claude_login.clone() {
            self.chat_widget.show_claude_login_pending(login_id);
            return;
        }
        if (self.account_ui.picker_request.is_some()
            && self.chat_widget.account_picker_loading_visible())
            || self.account_ui.selection_request.is_some()
            || self.account_ui.login_request.is_some()
            || self.account_ui.nickname_request.is_some()
        {
            return;
        }
        let serial = Uuid::new_v4();
        self.account_ui.picker_request = Some(serial);
        self.chat_widget.show_account_picker_loading();
        let request_id = server.next_request_id();
        let handle = server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(/*secs*/ 30),
                handle.request_typed::<ThreadAccountListResponse>(
                    ClientRequest::ThreadAccountList {
                        request_id,
                        params: ThreadAccountListParams {
                            thread_id: thread_id.to_string(),
                        },
                    },
                ),
            )
            .await
            .map_err(|_| "Account discovery timed out; try /account again".to_owned())
            .and_then(|result| result.map_err(|error| error.to_string()));
            tx.send(AppEvent::AccountPickerLoaded {
                thread_id,
                serial,
                result,
            });
        });
    }

    pub(super) fn apply_account_picker(
        &mut self,
        thread_id: ThreadId,
        serial: Uuid,
        result: Result<ThreadAccountListResponse, String>,
    ) {
        if self.account_ui.picker_request != Some(serial) {
            return;
        }
        self.account_ui.picker_request = None;
        if self.current_displayed_thread_id() != Some(thread_id)
            || !self.chat_widget.finish_account_picker_loading()
        {
            return;
        }
        match result {
            Ok(accounts) => self.chat_widget.show_account_picker(thread_id, accounts),
            Err(error) => self
                .chat_widget
                .add_error_message(format!("Cannot list accounts: {error}")),
        }
    }

    pub(super) fn select_account(
        &mut self,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        account_id: String,
        provider: AccountProvider,
    ) {
        if self.account_ui.selection_request.is_some()
            || self.account_ui.claude_login.is_some()
            || self.account_ui.login_request.is_some()
        {
            return;
        }
        if provider == AccountProvider::OpenRouter && self.chat_widget.openrouter_request_pending()
        {
            return;
        }
        self.account_ui.selection_request = Some(thread_id);
        if provider == AccountProvider::OpenRouter {
            self.chat_widget
                .show_openrouter_account_selection(&account_id);
        }
        self.account_ui.picker_request = None;
        let request_id = server.next_request_id();
        let handle = server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(/*secs*/ 30),
                handle.request_typed::<ThreadAccountSelectResponse>(
                    ClientRequest::ThreadAccountSelect {
                        request_id,
                        params: ThreadAccountSelectParams {
                            thread_id: thread_id.to_string(),
                            account_id,
                            provider: Some(provider),
                        },
                    },
                ),
            )
            .await
            .map_err(|_| "Selection not confirmed; reopen /account to check".to_owned())
            .and_then(|result| {
                result.map_err(|error| {
                    if provider == AccountProvider::OpenRouter {
                        crate::openrouter::error_message(&error.into())
                    } else {
                        error.to_string()
                    }
                })
            });
            tx.send(AppEvent::AccountSelectionFinished { thread_id, result });
        });
    }

    pub(super) fn finish_account_selection(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        result: Result<ThreadAccountSelectResponse, String>,
    ) {
        self.account_ui.selection_request = None;
        if self.current_displayed_thread_id() == Some(thread_id)
            && self
                .chat_widget
                .finish_openrouter_account_selection(&result)
        {
            return;
        }
        match result {
            Ok(response) => {
                if response.conversation_changed {
                    if response.pending {
                        self.account_ui
                            .pending
                            .insert(thread_id, response.account_id.clone());
                    } else {
                        self.account_ui.pending.remove(&thread_id);
                    }
                }
                // The ChatGPT default is the server's active login, not configuration.
                if response.default_saved
                    && response.provider == AccountProvider::ClaudeSubscription
                {
                    self.config.wrangler.default_claude_account = Some(response.account_id.clone());
                }
                if self.current_displayed_thread_id() != Some(thread_id) {
                    return;
                }
                if response.conversation_changed {
                    self.rate_limit_hard_stop_generation =
                        self.rate_limit_hard_stop_generation.wrapping_add(1);
                    self.rate_limit_refresh_state.invalidate_recovery();
                    self.last_thread_usage_status_cell = None;
                    self.pending_thread_usage_history_refresh = false;
                    self.refresh_conversation_account(server, thread_id);
                    self.chat_widget
                        .refresh_openrouter_status(crate::openrouter::StatusRefresh::Now);
                }
                self.chat_widget.add_info_message(
                    if !response.conversation_changed {
                        "Default account changed for new conversations. This conversation keeps its current provider."
                    } else if response.pending {
                        "Account switch pending. This conversation and its agents will switch between model/tool steps."
                    } else {
                        "Conversation account changed. Running agents will follow between model/tool steps."
                    }.to_owned(),
                    (!response.default_saved)
                        .then(|| "Could not save the default for new conversations.".to_owned()),
                );
            }
            Err(error) if self.current_displayed_thread_id() == Some(thread_id) => self
                .chat_widget
                .add_error_message(format!("Cannot change account: {error}")),
            Err(error) => {
                tracing::warn!(%thread_id, %error, "cannot change background conversation account");
            }
        }
    }

    pub(super) fn add_claude_account(&mut self, server: &mut AppServerSession) {
        if self.account_ui.login_request.is_some() || self.account_ui.claude_login.is_some() {
            return;
        }
        let start_id = ClaudeLoginStartId(Uuid::new_v4());
        self.account_ui.login_request = Some(start_id);
        self.chat_widget
            .add_info_message("Starting Claude sign-in…".to_owned(), /*hint*/ None);
        let request_id = server.next_request_id();
        let handle = server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(/*secs*/ 30),
                handle.request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                    request_id,
                    params: codex_app_server_protocol::LoginAccountParams::ClaudeSubscription,
                }),
            )
            .await
            .map_err(|_| "Sign-in start not confirmed; reopen /account before retrying".to_owned())
            .and_then(|result| result.map_err(|error| error.to_string()));
            tx.send(AppEvent::ClaudeLoginStarted { start_id, result });
        });
    }

    pub(super) fn finish_claude_login_start(
        &mut self,
        start_id: ClaudeLoginStartId,
        result: Result<LoginAccountResponse, String>,
    ) {
        if self.account_ui.login_request != Some(start_id) {
            return;
        }
        self.account_ui.login_request = None;
        match result {
            Ok(LoginAccountResponse::ClaudeSubscription { login_id, auth_url }) => {
                self.account_ui.claude_login = Some(login_id.clone());
                if let Some(index) = self
                    .account_ui
                    .early_login_completions
                    .iter()
                    .position(|completion| completion.login_id.as_ref() == Some(&login_id))
                {
                    let Some(completion) = self.account_ui.early_login_completions.remove(index)
                    else {
                        return;
                    };
                    self.account_ui.early_login_completions.clear();
                    self.finish_claude_login(&completion);
                    return;
                }
                self.account_ui.early_login_completions.clear();
                if let Err(error) = webbrowser::open(&auth_url) {
                    self.chat_widget.add_error_message(format!(
                        "Cannot open browser: {error}. Open this URL to sign in: {auth_url}"
                    ));
                }
                self.chat_widget.show_claude_login_pending(login_id);
            }
            Ok(_) => {
                self.account_ui.early_login_completions.clear();
                self.chat_widget.add_error_message(
                    "Server returned another provider's sign-in flow".to_owned(),
                );
            }
            Err(error) => {
                self.account_ui.early_login_completions.clear();
                self.chat_widget
                    .add_error_message(format!("Cannot start Claude sign-in: {error}"));
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClaudeLoginStartId(Uuid);
