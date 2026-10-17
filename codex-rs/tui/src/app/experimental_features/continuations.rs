//! Deferred feature persistence for future threads.

use super::*;

impl App {
    pub(super) async fn enable_feature_for_new_threads_continuation(
        &mut self,
        _tui: &mut tui::Tui,
        app_server: &AppServerSession,
        label: &'static str,
        edits: Vec<codex_app_server_protocol::ConfigEdit>,
    ) {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        self.request_server(
                app_server,
                UiRequestScope::Connection,
                UiRequestKind::Mutation,
                "Save feature for new threads",
                move |handle| crate::config_update::write_config_batch_typed(handle, edits),
                move |app, _, result| {
                    let notice: Box<dyn HistoryCell> = match result {
                        Ok(response) if response.status == WriteStatus::Ok => Box::new(history_cell::new_warning_event(format!(
                            "{label} setting saved on the server for new threads. This thread is unchanged. Project or task settings may override it."
                        ))),
                        Ok(response) => {
                            app.app_event_tx.send(AppEvent::FollowTranscript);
                            Box::new(history_cell::new_error_event(format!("{label} setting was saved but is overridden: {}", overridden_write_message(&response))))
                        },
                        Err(error) => {
                            app.app_event_tx.send(AppEvent::FollowTranscript);
                            Box::new(history_cell::new_error_event(format!("Failed to save {label} setting: {error}")))
                        },
                    };
                    app.app_event_tx.send(AppEvent::InsertHistoryCell(notice));
                    Ok(())
                },
            );
    }
}
