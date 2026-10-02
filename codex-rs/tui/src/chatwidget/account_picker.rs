//! Account discovery stays in a dismissible picker while the server inventories logins.

use super::*;

const ACCOUNT_PICKER_LOADING_VIEW: &str = "account-picker-loading";
const CLAUDE_LOGIN_PENDING_VIEW: &str = "claude-login";

impl ChatWidget {
    pub(crate) fn finish_claude_login_pending(&mut self) {
        self.bottom_pane
            .dismiss_view_by_id(CLAUDE_LOGIN_PENDING_VIEW);
    }

    pub(crate) fn show_claude_login_pending(&mut self, login_id: String) {
        self.bottom_pane
            .show_selection_view(crate::bottom_pane::SelectionViewParams {
                view_id: Some(CLAUDE_LOGIN_PENDING_VIEW),
                title: Some("Sign in to Claude".to_owned()),
                subtitle: Some(
                    "Complete sign-in in your browser. Existing conversations keep their accounts."
                        .to_owned(),
                ),
                items: vec![crate::bottom_pane::SelectionItem {
                    name: "Cancel sign-in".to_owned(),
                    actions: vec![Box::new(move |tx| {
                        tx.send(crate::app_event::AppEvent::CancelClaudeLogin {
                            login_id: login_id.clone(),
                        })
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }],
                footer_hint: Some(standard_popup_hint_line()),
                ..Default::default()
            });
    }

    pub(crate) fn show_account_picker_loading(&mut self) {
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
