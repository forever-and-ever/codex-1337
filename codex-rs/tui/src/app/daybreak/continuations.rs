//! Deferred Daybreak persistence and accepted preference projection.

use super::App;
use super::AppServerSession;
use super::Arc;
use super::ThreadId;
use super::Uuid;
use crate::app_server_session::typed_requests::SessionRpc;
use codex_app_server_protocol::RequestId;

impl App {
    pub(in crate::app) fn persist_daybreak_selection_continuation(
        &mut self,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        enabled: bool,
        after_save: impl FnOnce(&mut App) + Send + 'static,
    ) {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        use codex_app_server_protocol::ThreadMetadataUpdateParams;
        use codex_app_server_protocol::ThreadMetadataUpdateResponse;
        use codex_app_server_protocol::ThreadReadParams;
        use codex_app_server_protocol::ThreadReadResponse;

        let cwd = self
            .chat_widget
            .config_ref()
            .cwd
            .to_string_lossy()
            .into_owned();
        let store = self
            .thread_event_channels
            .get(&thread_id)
            .map(|channel| Arc::clone(&channel.store));
        let read_store = store.clone();
        self.request_server(
            server,
            UiRequestScope::Thread(thread_id),
            UiRequestKind::ReadOnly,
            "Read thread for Daybreak",
            move |handle| async move {
                let response: ThreadReadResponse = handle
                    .thread_read(
                        RequestId::String(format!("tui-daybreak-read-{}", Uuid::new_v4())),
                        ThreadReadParams {
                            thread_id: thread_id.to_string(),
                            include_turns: false,
                        },
                    )
                    .await?;
                if response.thread.ephemeral
                    && let Some(store) = read_store
                    && let Some(session) = store.lock().await.session.as_mut()
                {
                    session.daybreak_enabled = enabled;
                }
                Ok(response.thread.ephemeral)
            },
            move |app, server, result| {
                match result {
                    Err(error) => app.chat_widget.add_error_message(format!(
                        "Failed to read the thread to save Daybreak: {error}"
                    )),
                    Ok(true) => {
                        app.commit_daybreak_selection(server, thread_id, enabled, cwd, after_save);
                    }
                    Ok(false) => {
                        app.request_server(
                            server,
                            UiRequestScope::Thread(thread_id),
                            UiRequestKind::Mutation,
                            "Save thread Daybreak",
                            move |handle| async move {
                                let _: ThreadMetadataUpdateResponse = handle
                                    .thread_metadata_update(
                                        RequestId::String(format!(
                                            "tui-daybreak-save-{}",
                                            Uuid::new_v4()
                                        )),
                                        ThreadMetadataUpdateParams {
                                            theme: None,
                                            thread_id: thread_id.to_string(),
                                            daybreak_enabled: Some(enabled),
                                            project_id: None,
                                            git_info: None,
                                            comms: None,
                                        },
                                    )
                                    .await?;
                                if let Some(store) = store
                                    && let Some(session) = store.lock().await.session.as_mut()
                                {
                                    session.daybreak_enabled = enabled;
                                }
                                Ok(())
                            },
                            move |app, server, result| {
                                match result {
                                    Ok(()) => app.commit_daybreak_selection(
                                        server, thread_id, enabled, cwd, after_save,
                                    ),
                                    Err(error) => app.chat_widget.add_error_message(format!(
                                        "Failed to save Daybreak for this thread: {error}"
                                    )),
                                }
                                Ok(())
                            },
                        );
                    }
                }
                Ok(())
            },
        );
    }

    pub(super) fn commit_daybreak_selection(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        enabled: bool,
        cwd: String,
        after_save: impl FnOnce(&mut App) + Send + 'static,
    ) {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        if self.primary_thread_id == Some(thread_id)
            && let Some(session) = self.primary_session_configured.as_mut()
        {
            session.daybreak_enabled = enabled;
        }
        if self.active_thread_id == Some(thread_id) {
            self.chat_widget.set_daybreak_enabled(enabled);
            self.refresh_status_line();
            self.chat_widget.add_info_message(
                format!(
                    "Daybreak {}. Applies to new turns.",
                    if enabled { "on" } else { "off" }
                ),
                None,
            );
        }
        after_save(self);
        self.queue_model_defaults(
            server,
            vec![crate::config_update::replace_config_value(
                "daybreak",
                serde_json::json!(enabled),
            )],
            "default Daybreak preference",
            move |app, server, result| {
                if let Err(error) = result {
                    app.chat_widget.add_error_message(format!(
                        "Failed to save the default Daybreak preference: {error}"
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
                        move |app, _, result| {
                            match result {
                                Ok(response) => {
                                    if app.current_displayed_thread_id() == Some(thread_id) {
                                        app.config.daybreak_enabled = response
                                            .config
                                            .additional
                                            .get("daybreak")
                                            .and_then(serde_json::Value::as_bool)
                                            .unwrap_or(false);
                                    }
                                }
                                Err(error) => app.chat_widget.add_error_message(format!(
                                    "Failed to read the default Daybreak preference: {error}"
                                )),
                            }
                            Ok(())
                        },
                    );
                }
                Ok(())
            },
        );
    }
}
