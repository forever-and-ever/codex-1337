//! Settings-adjacent popup surfaces for `ChatWidget`.
//!
//! This keeps theme, personality, and experimental-feature UI out of the main
//! orchestration module without changing their event wiring.

use super::*;

impl ChatWidget {
    pub(crate) fn show_account_picker(
        &mut self,
        thread_id: ThreadId,
        response: codex_app_server_protocol::ThreadAccountListResponse,
    ) {
        if response.accounts.is_empty() {
            self.add_info_message(
                "No saved accounts. Run `codex login` to add one.".to_owned(),
                None,
            );
            return;
        }
        let items = response
            .accounts
            .into_iter()
            .map(|account| {
                let name = account
                    .alias
                    .clone()
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
                SelectionItem {
                    name,
                    description,
                    is_current,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SelectAccount {
                            thread_id,
                            account_id: account.id.clone(),
                        })
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select Account".to_owned()),
            subtitle: Some("Conversation + agents; also the new-conversation default.".to_owned()),
            items,
            footer_hint: Some(standard_popup_hint_line()),
            ..Default::default()
        });
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

    pub(crate) fn open_personality_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Personality selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }
        if !self.current_model_supports_personality() {
            let current_model = self.current_model();
            self.add_error_message(format!(
                "Current model ({current_model}) doesn't support personalities. Try /model to pick a different model."
            ));
            return;
        }
        self.open_personality_popup_for_current_model();
    }

    fn open_personality_popup_for_current_model(&mut self) {
        let current_personality = self.config.personality.unwrap_or(Personality::Friendly);
        let personalities = [Personality::Friendly, Personality::Pragmatic];
        let supports_personality = self.current_model_supports_personality();

        let items: Vec<SelectionItem> = personalities
            .into_iter()
            .map(|personality| {
                let name = Self::personality_label(personality).to_string();
                let description = Some(Self::personality_description(personality).to_string());
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::CodexOp(AppCommand::override_turn_context(
                        /*cwd*/ None,
                        /*approval_policy*/ None,
                        /*approvals_reviewer*/ None,
                        /*permission_profile*/ None,
                        /*active_permission_profile*/ None,
                        /*windows_sandbox_level*/ None,
                        /*model*/ None,
                        /*effort*/ None,
                        /*summary*/ None,
                        /*service_tier*/ None,
                        /*collaboration_mode*/ None,
                        Some(personality),
                    )));
                    tx.send(AppEvent::UpdatePersonality(personality));
                    tx.send(AppEvent::PersistPersonalitySelection { personality });
                })];
                SelectionItem {
                    name,
                    description,
                    is_current: current_personality == personality,
                    is_disabled: !supports_personality,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        let mut header = ColumnRenderable::new();
        header.push(Line::from("Select Personality".bold()));
        header.push(Line::from("Choose a communication style for Codex.".dim()));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
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

    fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "None",
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    fn personality_description(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "No personality instructions.",
            Personality::Friendly => "Warm, collaborative, and helpful.",
            Personality::Pragmatic => "Concise, task-focused, and direct.",
        }
    }
}
