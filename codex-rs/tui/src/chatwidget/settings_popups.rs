//! Settings-adjacent popup surfaces for `ChatWidget`.
//!
//! This keeps theme and experimental-feature UI out of the main
//! orchestration module without changing their event wiring.

use super::*;

impl ChatWidget {
    pub(crate) fn show_account_picker(
        &mut self,
        thread_id: ThreadId,
        response: codex_app_server_protocol::ThreadAccountListResponse,
    ) {
        use codex_app_server_protocol::SubscriptionAccountProvider;
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
                            provider: SubscriptionAccountProvider::Openai,
                        })
                    })],
                    secondary_action: Some(crate::bottom_pane::SelectionSecondaryAction {
                        key: key_hint::ctrl(KeyCode::Char('r')),
                        footer_hint: "enter select · ctrl+r rename · esc back".into(),
                        action: Box::new(move |tx| {
                            tx.send(AppEvent::RenameAccountPrompt {
                                thread_id,
                                provider: SubscriptionAccountProvider::Openai,
                                account_id: rename_id.clone(),
                                nickname: rename_name.clone(),
                            })
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
                        provider: SubscriptionAccountProvider::ClaudeSubscription,
                    })
                })],
                secondary_action: Some(crate::bottom_pane::SelectionSecondaryAction {
                    key: key_hint::ctrl(KeyCode::Char('r')),
                    footer_hint: "enter select · ctrl+r rename · esc back".into(),
                    action: Box::new(move |tx| {
                        tx.send(AppEvent::RenameAccountPrompt {
                            thread_id,
                            provider: SubscriptionAccountProvider::ClaudeSubscription,
                            account_id: rename_id.clone(),
                            nickname: rename_name.clone(),
                        })
                    }),
                }),
                dismiss_on_select: true,
                ..Default::default()
            }
        }));
        items.push(SelectionItem {
            name: "Add Claude account".to_owned(),
            description: Some("Sign in with your Claude subscription".to_owned()),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::AddClaudeAccount { thread_id })
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
                Some(SubscriptionAccountProvider::Openai) => "ChatGPT changes this conversation; Claude sets its new-conversation default.",
                Some(SubscriptionAccountProvider::ClaudeSubscription) => "Claude changes this conversation; ChatGPT sets its new-conversation default.",
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
        provider: codex_app_server_protocol::SubscriptionAccountProvider,
        account_id: String,
        nickname: Option<String>,
    ) {
        let title = match provider {
            codex_app_server_protocol::SubscriptionAccountProvider::Openai => {
                "Rename ChatGPT account"
            }
            codex_app_server_protocol::SubscriptionAccountProvider::ClaudeSubscription => {
                "Rename Claude account"
            }
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
                    })
                }),
            )
            .allow_empty_submit(),
        );
    }

    pub(super) fn open_theme_picker(&mut self) {
        let codex_home = codex_utils_home_dir::find_codex_home().ok();
        let params = crate::theme_picker::build_theme_picker_params(
            self.local_settings.tui.theme.as_deref(),
            codex_home.as_deref(),
            self.last_rendered_width.get(),
        );
        self.bottom_pane.show_selection_view(params);
    }

    pub(crate) fn open_experimental_popup(&mut self) {
        let Some(thread_id) = self.thread_id() else {
            self.add_info_message(
                "Experimental features are unavailable until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        };
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.app_event_tx.send(AppEvent::FetchExperimentalFeatures {
            thread_id,
            response_tx,
        });
        let view = ExperimentalFeaturesView::new(
            Vec::new(),
            thread_id,
            Some(response_rx),
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }
}
