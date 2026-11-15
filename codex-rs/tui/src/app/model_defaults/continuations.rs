//! Deferred persistence and completion of model defaults.

use super::App;
use super::AppServerSession;
use super::ConfigEdit;
use super::Result;
use super::WriteStatus;

impl App {
    pub(in crate::app) fn queue_model_defaults<A>(
        &mut self,
        server: &AppServerSession,
        edits: Vec<ConfigEdit>,
        setting: &'static str,
        apply: A,
    ) where
        A: FnOnce(
                &mut App,
                &mut AppServerSession,
                std::result::Result<(), &super::super::turn_submissions::SubmissionFailure>,
            ) -> Result<()>
            + Send
            + 'static,
    {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        self.request_server(
                server,
                UiRequestScope::Connection,
                UiRequestKind::Mutation,
                setting,
                move |handle| crate::config_update::write_config_batch_typed(handle, edits),
                move |app, server, result| {
                    let result = result.map(|response| {
                        if response.status == WriteStatus::OkOverridden {
                            app.chat_widget.add_warning_message(format!("Saved {setting}, but a higher-priority configuration layer overrides the saved value."));
                        }
                    });
                    apply(app, server, result)
                },
            );
    }
}

impl App {
    pub(in crate::app) fn model_selection_changes_current_model(&self, model: &str) -> bool {
        self.chat_widget.automatic_model_switch_pending()
            || self.chat_widget.current_model() != model
            || self.chat_widget.current_collaboration_mode().model() != model
    }
}
