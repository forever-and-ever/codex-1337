//! Delay bare-launch thread creation until the composer has a submitted input.

use super::App;
use super::AppServerSession;
use super::Arc;
use super::ChatWidget;
use super::Config;
use super::FileSearchManager;
use super::ModelCatalog;
use super::ProviderModelChoice;
use super::Result;
use super::ThreadInputState;
use super::startup;
use super::tui;
use crate::chatwidget::SessionDraft;

pub(super) struct ParkedSessionDraft {
    pub(super) config: Config,
    pub(super) catalog: Arc<ModelCatalog>,
    pub(super) input: ThreadInputState,
}

impl App {
    pub(super) async fn restore_parked_session_draft(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
    ) -> Result<bool> {
        if self.agents_overview.parked_draft.is_none() {
            return Ok(false);
        }
        self.enqueue_parked_draft_restore(server).await;
        tui.frame_requester().schedule_frame();
        Ok(true)
    }

    pub(super) async fn commit_parked_session_draft(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
    ) -> Result<()> {
        self.reset_for_thread_switch(tui)?;
        self.store_active_thread_receiver().await;
        self.reset_thread_event_state().await;
        let Some(draft) = self.agents_overview.parked_draft.take() else {
            return Ok(());
        };
        self.config = draft.config;
        self.model_catalog = draft.catalog;
        server.set_available_models(self.model_catalog.models.clone());
        self.local_settings = self.local_settings.reloaded(&self.config);
        let init =
            self.chatwidget_init_for_forked_or_resumed_thread(tui, self.config.clone(), None);
        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
        self.chat_widget.restore_thread_input_state(
            Some(draft.input),
            crate::chatwidget::ThreadInputStateRestoreMode {
                preserve_in_flight_turn: false,
            },
        );
        self.chat_widget.restore_session_draft();
        self.file_search =
            FileSearchManager::new(self.config.cwd.to_path_buf(), self.app_event_tx.clone());
        self.refresh_skills(server);
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    pub(super) fn start_draft_session(&mut self, server: &AppServerSession) {
        if self.chat_widget.session_draft != Some(SessionDraft::Starting)
            || self.pending_startup_thread_start
            || self.chat_widget.thread_id().is_some()
        {
            return;
        }
        self.config = self.chat_widget.config_ref().clone();
        self.config.model = Some(self.chat_widget.current_model().to_owned());
        self.config.model_reasoning_effort = self.chat_widget.current_reasoning_effort();
        self.pending_startup_thread_start = true;
        let mut launch_choices = crate::app_server_session::StartupLaunchChoices::default();
        launch_choices.model.clone_from(&self.config.model);
        launch_choices.effort = self
            .config
            .model_reasoning_effort
            .as_ref()
            .and_then(|effort| serde_json::to_value(effort).ok());
        startup::spawn_startup_thread_start(
            server,
            self.local_settings.clone(),
            self.config.clone(),
            launch_choices,
            self.app_event_tx.clone(),
            Some(self.config.model_provider_id.clone()),
        );
    }

    pub(super) fn select_draft_provider(
        &mut self,
        server: &mut AppServerSession,
        choice: ProviderModelChoice,
    ) {
        if self.chat_widget.session_draft != Some(SessionDraft::Editing) {
            self.chat_widget.add_error_message(
                "Wait for the conversation to start before changing provider.".into(),
            );
            return;
        }
        if let Err(error) = self.chat_widget.select_draft_provider(&choice) {
            self.chat_widget.add_error_message(error);
            return;
        }
        self.config = self.chat_widget.config_ref().clone();
        self.model_catalog = self.chat_widget.model_catalog();
        server.set_available_models(self.model_catalog.models.clone());
        self.queue_model_defaults(
            server,
            crate::config_update::build_model_selection_edits(
                &choice.provider.id,
                &choice.model.model,
                Some(choice.effort),
            ),
            "default provider, model, and reasoning effort",
            |app, _, result| {
                if let Err(error) = result {
                    app.chat_widget.add_error_message(format!(
                        "Provider selected for this draft, but failed to save defaults: {error}"
                    ));
                }
                Ok(())
            },
        );
    }
}
