//! Persist the user's Daybreak selection for the active thread and new threads.

use super::*;

impl App {
    pub(super) async fn persist_daybreak_selection(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        enabled: bool,
    ) {
        use super::turn_submissions::ui_requests::{UiRequestKind, UiRequestScope};
        use codex_app_server_protocol::{
            ThreadMetadataUpdateParams, ThreadMetadataUpdateResponse, ThreadReadParams,
            ThreadReadResponse,
        };
        if !self
            .chat_widget
            .config_ref()
            .features
            .enabled(Feature::CliDaybreak)
            || self.active_thread_id != Some(thread_id)
        {
            return;
        }
        if enabled
            && (!self.chat_widget.daybreak_turn_eligible(enabled)
                || !crate::daybreak::available(&self.chat_widget.model_catalog().models))
        {
            self.chat_widget.add_error_message(
                "Daybreak availability could not be confirmed for this account.".into(),
            );
            return;
        }
        let store = self
            .thread_event_channels
            .get(&thread_id)
            .map(|channel| Arc::clone(&channel.store));
        self.request_server(
            app_server,
            UiRequestScope::Thread(thread_id),
            UiRequestKind::Mutation,
            "Save thread Daybreak",
            move |handle| async move {
                let response: ThreadReadResponse = handle
                    .request_typed(ClientRequest::ThreadRead {
                        request_id: codex_app_server_protocol::RequestId::String(format!(
                            "tui-daybreak-read-{}",
                            uuid::Uuid::new_v4()
                        )),
                        params: ThreadReadParams {
                            thread_id: thread_id.to_string(),
                            include_turns: false,
                        },
                    })
                    .await?;
                if !response.thread.ephemeral {
                    let _: ThreadMetadataUpdateResponse = handle
                        .request_typed(ClientRequest::ThreadMetadataUpdate {
                            request_id: codex_app_server_protocol::RequestId::String(format!(
                                "tui-daybreak-save-{}",
                                uuid::Uuid::new_v4()
                            )),
                            params: ThreadMetadataUpdateParams {
                                thread_id: thread_id.to_string(),
                                daybreak_enabled: Some(enabled),
                                project_id: None,
                                git_info: None,
                                comms: None,
                            },
                        })
                        .await?;
                }
                if let Some(store) = store
                    && let Some(session) = store.lock().await.session.as_mut()
                {
                    session.daybreak_enabled = enabled;
                }
                Ok(())
            },
            move |app, server, result| {
                if let Err(error) = result {
                    app.chat_widget.add_error_message(format!(
                        "Failed to read the thread or save Daybreak: {}",
                        super::model_defaults::request_failure_message(error)
                    ));
                    return Ok(());
                }
                if app.primary_thread_id == Some(thread_id)
                    && let Some(session) = app.primary_session_configured.as_mut()
                {
                    session.daybreak_enabled = enabled;
                }
                if app.active_thread_id == Some(thread_id) {
                    app.chat_widget.set_daybreak_enabled(enabled);
                    app.refresh_status_line();
                    app.chat_widget.add_info_message(
                        format!(
                            "Daybreak {}. Applies to new turns.",
                            if enabled { "on" } else { "off" }
                        ),
                        None,
                    );
                }
                let cwd = app
                    .chat_widget
                    .config_ref()
                    .cwd
                    .to_string_lossy()
                    .into_owned();
                app.queue_model_defaults(
                    server,
                    vec![crate::config_update::replace_config_value(
                        "daybreak",
                        serde_json::json!(enabled),
                    )],
                    "default Daybreak preference",
                    move |app, server, result| {
                        if let Err(error) = result {
                            app.chat_widget.add_error_message(format!(
                                "Failed to save the default Daybreak preference: {}",
                                super::model_defaults::request_failure_message(error)
                            ));
                        } else {
                            app.request_server(
                                server,
                                UiRequestScope::Connection,
                                UiRequestKind::ReadOnly,
                                "Read default Daybreak preference",
                                move |handle| {
                                    crate::config_update::read_effective_config_typed(handle, cwd)
                                },
                                |app, _, result| {
                                    match result {
                                        Ok(response) => {
                                            app.config.daybreak_enabled = response
                                                .config
                                                .additional
                                                .get("daybreak")
                                                .and_then(serde_json::Value::as_bool)
                                                .unwrap_or(false)
                                        }
                                        Err(error) => app.chat_widget.add_error_message(format!(
                                            "Failed to read the default Daybreak preference: {}",
                                            super::model_defaults::request_failure_message(error)
                                        )),
                                    }
                                    Ok(())
                                },
                            );
                        }
                        Ok(())
                    },
                );
                Ok(())
            },
        );
    }
}
