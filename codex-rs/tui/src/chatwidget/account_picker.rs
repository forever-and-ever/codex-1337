//! Account discovery stays in a dismissible picker while the server inventories logins.

use super::AppEvent;
use super::ChatWidget;
use super::CustomPromptView;
use super::KeyCode;
use super::SelectionItem;
use super::SelectionViewParams;
use super::StatusAccountDisplay;
use super::ThreadId;
use super::key_hint;
use super::standard_popup_hint_line;
use codex_app_server_protocol::AccountProvider;

const ACCOUNT_PICKER_LOADING_VIEW: &str = "account-picker-loading";
const CLAUDE_LOGIN_PENDING_VIEW: &str = "claude-login";

impl ChatWidget {
    pub(crate) fn set_status_account_display(&mut self, display: Option<StatusAccountDisplay>) {
        if let Some(account) = self.loaded_account_mut() {
            account.display = display;
        }
    }

    pub(crate) fn show_account_picker(
        &mut self,
        thread_id: ThreadId,
        response: codex_app_server_protocol::ThreadAccountListResponse,
    ) {
        if self.refuse_pending_account_operation() {
            return;
        }
        let mut openrouter_items = Vec::new();
        Self::append_openrouter_accounts(thread_id, &response, &mut openrouter_items);
        let mut items: Vec<SelectionItem> = response
            .accounts
            .into_iter()
            .map(|account| {
                let name = account
                    .nickname
                    .clone()
                    .or(account.alias.clone())
                    .or(account.email.clone())
                    .unwrap_or_else(|| account.id.clone());
                let email = account.email.filter(|email| email != &name);
                let pending = response.pending_account_id.as_ref() == Some(&account.id);
                let description = Some(
                    [
                        pending.then(|| "switch pending".to_owned()),
                        email,
                        account.plan,
                        Some(account.id.clone()),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · "),
                );
                let is_current = response.selected_account_id.as_ref() == Some(&account.id);
                let rename_id = account.id.clone();
                let rename_name = account.nickname.clone();
                SelectionItem {
                    name: format!("ChatGPT · {name}"),
                    description,
                    is_current,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SelectAccount {
                            thread_id,
                            account_id: account.id.clone(),
                            provider: AccountProvider::Openai,
                        });
                    })],
                    secondary_action: Some(crate::bottom_pane::SelectionSecondaryAction {
                        key: key_hint::ctrl(KeyCode::Char('r')),
                        footer_hint: "enter select · ctrl+r rename · esc back".into(),
                        action: Box::new(move |tx| {
                            tx.send(AppEvent::RenameAccountPrompt {
                                thread_id,
                                provider: AccountProvider::Openai,
                                account_id: rename_id.clone(),
                                nickname: rename_name.clone(),
                            });
                        }),
                    }),
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        items.extend(response.claude_accounts.into_iter().map(|account| {
            let is_current = response.selected_claude_account_id.as_ref() == Some(&account.id);
            let pending = response.pending_claude_account_id.as_ref() == Some(&account.id);
            let name = account
                .nickname
                .clone()
                .or(account.email.clone())
                .unwrap_or_else(|| account.id.clone());
            let rename_id = account.id.clone();
            let rename_name = account.nickname.clone();
            SelectionItem {
                name: format!("Claude · {name}"),
                description: Some(
                    [
                        pending.then(|| "switch pending".to_owned()),
                        account
                            .status
                            .as_ref()
                            .map(|status| format!("unavailable: {status}")),
                        account.email.clone().filter(|email| email != &name),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · "),
                )
                .filter(|description| !description.is_empty()),
                is_current,
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::SelectAccount {
                        thread_id,
                        account_id: account.id.clone(),
                        provider: AccountProvider::ClaudeSubscription,
                    });
                })],
                secondary_action: Some(crate::bottom_pane::SelectionSecondaryAction {
                    key: key_hint::ctrl(KeyCode::Char('r')),
                    footer_hint: "enter select · ctrl+r rename · esc back".into(),
                    action: Box::new(move |tx| {
                        tx.send(AppEvent::RenameAccountPrompt {
                            thread_id,
                            provider: AccountProvider::ClaudeSubscription,
                            account_id: rename_id.clone(),
                            nickname: rename_name.clone(),
                        });
                    }),
                }),
                dismiss_on_select: true,
                ..Default::default()
            }
        }));
        items.extend(openrouter_items);
        items.push(SelectionItem {
            name: "Add Claude account".to_owned(),
            description: Some("Sign in with your Claude subscription".to_owned()),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::AddClaudeAccount);
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
        if let Some(error) = response.claude_management_error {
            items.push(SelectionItem {
                name: "Cannot read Claude accounts".to_owned(),
                disabled_reason: Some(error),
                ..Default::default()
            });
        }
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Accounts".to_owned()),
            subtitle: Some(match response.conversation_provider {
                Some(AccountProvider::Openai) => "ChatGPT changes this conversation; other accounts set their new-conversation defaults.",
                Some(AccountProvider::ClaudeSubscription) => "Claude changes this conversation; other accounts set their new-conversation defaults.",
                Some(AccountProvider::OpenRouter) => "OpenRouter changes this conversation; other accounts set their new-conversation defaults.",
                None => "Selections set new-conversation defaults; this conversation uses another provider.",
            }.to_owned()),
            items,
            footer_hint: Some(standard_popup_hint_line()),
            ..Default::default()
        });
    }

    pub(crate) fn show_account_nickname_prompt(
        &mut self,
        thread_id: ThreadId,
        provider: AccountProvider,
        account_id: String,
        nickname: Option<String>,
    ) {
        let title = match provider {
            AccountProvider::OpenRouter => {
                self.add_error_message("OpenRouter keys are named by their key name".to_owned());
                return;
            }
            AccountProvider::Openai => "Rename ChatGPT account",
            AccountProvider::ClaudeSubscription => "Rename Claude account",
        };
        let tx = self.app_event_tx.clone();
        self.bottom_pane.show_text_prompt(
            CustomPromptView::new(
                title.to_owned(),
                "Nickname (empty clears)".to_owned(),
                nickname.unwrap_or_default(),
                Some("Display only; empty clears; account routing never changes".to_owned()),
                Box::new(move |nickname| {
                    tx.send(AppEvent::SetAccountNickname {
                        thread_id,
                        provider,
                        account_id: account_id.clone(),
                        nickname,
                    });
                }),
            )
            .allow_empty_submit(),
        );
    }

    pub(crate) fn finish_claude_login_pending(&mut self) {
        self.bottom_pane
            .dismiss_view_by_id(CLAUDE_LOGIN_PENDING_VIEW);
    }

    pub(crate) fn show_claude_login_pending(&mut self, login_id: String) {
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(CLAUDE_LOGIN_PENDING_VIEW),
            title: Some("Sign in to Claude".to_owned()),
            subtitle: Some(
                "Complete sign-in in your browser. Existing conversations keep their accounts."
                    .to_owned(),
            ),
            items: vec![SelectionItem {
                name: "Cancel sign-in".to_owned(),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::CancelClaudeLogin {
                        login_id: login_id.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            }],
            footer_hint: Some(standard_popup_hint_line()),
            ..Default::default()
        });
    }

    pub(crate) fn show_account_picker_loading(&mut self) {
        if self.refuse_pending_account_operation() {
            return;
        }
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(ACCOUNT_PICKER_LOADING_VIEW),
            title: Some("Accounts".into()),
            items: vec![SelectionItem {
                name: "Loading accounts…".into(),
                is_disabled: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        });
    }

    pub(crate) fn account_picker_loading_visible(&self) -> bool {
        self.bottom_pane.active_view_id() == Some(ACCOUNT_PICKER_LOADING_VIEW)
    }

    pub(crate) fn finish_account_picker_loading(&mut self) -> bool {
        self.bottom_pane
            .dismiss_active_view_if_id(ACCOUNT_PICKER_LOADING_VIEW)
    }
}
