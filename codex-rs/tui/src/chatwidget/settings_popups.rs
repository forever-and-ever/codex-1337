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
