//! A fresh launch is a composer, not a server conversation, until it receives work.

use super::AppEvent;
use super::Arc;
use super::ChatWidget;
use super::Insets;
use super::Line;
use super::RenderableExt;
use super::RenderableItem;
use super::SlashCommand;
use super::Stylize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionDraft {
    Editing,
    Starting,
    AwaitingSignIn(codex_protocol::sign_in_required::AccountProvider),
}

impl ChatWidget {
    pub(super) fn defer_draft_command(&mut self, command: SlashCommand) -> bool {
        if self.session_draft.is_none()
            || command.available_when_thread_unavailable()
                && (command != SlashCommand::Account
                    || matches!(self.session_draft, Some(SessionDraft::AwaitingSignIn(_))))
            || matches!(command, SlashCommand::Model | SlashCommand::Reload)
        {
            return false;
        }
        // Inline-argument and validation-sensitive commands leave their full draft in the
        // composer. Only a consumed bare command must be restored before confirming startup.
        if self.bottom_pane.composer_text().is_empty() {
            let message =
                self.user_message_from_submission(format!("/{}", command.command()), Vec::new());
            self.restore_user_message_to_composer(message);
        }
        self.input_queue_frontend.startup_submission =
            Some(self.bottom_pane.composer_draft_snapshot());
        self.request_draft_session();
        true
    }

    pub(super) fn request_usage(&mut self, args: String) {
        self.app_event_tx.send(AppEvent::Usage { args });
    }

    pub(crate) fn dispatch_usage(&mut self, args: String) {
        if args.is_empty() {
            self.dispatch_command(SlashCommand::Usage);
        } else {
            self.dispatch_command_with_args(SlashCommand::Usage, args, Vec::new());
        }
    }

    pub(crate) fn request_draft_session(&mut self) {
        if self.session_draft == Some(SessionDraft::Editing) {
            self.session_draft = Some(SessionDraft::Starting);
            self.app_event_tx.send(AppEvent::StartDraftSession);
            self.request_redraw();
        }
    }

    pub(crate) fn restore_session_draft(&mut self) {
        self.session_draft = Some(SessionDraft::Editing);
        self.cancel_startup_submission();
        if let Some(composer) = self.drain_pending_messages_for_restore() {
            self.restore_composer_state(composer);
        }
        self.input_queue.recovered_queue = false;
        self.set_queue_submissions_until_session_configured(/*queue*/ false);
        self.refresh_pending_input_preview();
    }

    pub(crate) fn select_draft_provider(
        &mut self,
        choice: &crate::app::ProviderModelChoice,
    ) -> Result<(), String> {
        let provider = self
            .config
            .model_providers
            .get(&choice.provider.id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "Provider {} is not configured in this client.",
                    choice.provider.name
                )
            })?;
        self.config
            .model_provider_id
            .clone_from(&choice.provider.id);
        self.config.model_provider = provider;
        self.config.model = Some(choice.model.model.clone());
        self.config.model_reasoning_effort = Some(choice.effort.clone());
        self.config.service_tier = Some("default".into());
        self.account = crate::app_server_session::BootstrapAccount::AwaitingResume;
        self.active_account_label = None;
        self.rate_limit_snapshots_by_limit_id.clear();
        Arc::make_mut(&mut self.model_catalog).models = vec![choice.model.clone()];
        self.set_model(&choice.model.model);
        self.set_reasoning_effort(Some(choice.effort.clone()));
        // Invalidate any request for the previous provider before refreshing this catalog.
        let request_id = uuid::Uuid::new_v4();
        self.set_model_refresh_request(request_id);
        self.app_event_tx.send(AppEvent::FetchModels { request_id });
        Ok(())
    }

    pub(super) fn draft_provider_hint(&self) -> RenderableItem<'static> {
        let text = match self.session_draft {
            Some(SessionDraft::Editing) => vec![
                self.config.model_provider.name.clone().bold(),
                " · ".dim(),
                "/model".into(),
                " change provider or model".dim(),
            ],
            Some(SessionDraft::Starting) => vec!["Starting conversation…".dim()],
            Some(SessionDraft::AwaitingSignIn(_)) => {
                vec!["/account".into(), " sign in again".dim()]
            }
            None => return RenderableItem::Owned(Box::new(())),
        };
        RenderableItem::Owned(Box::new(Line::from(text))).inset(Insets::tlbr(
            /*top*/ 0, /*left*/ 2, /*bottom*/ 0, /*right*/ 2,
        ))
    }
}
