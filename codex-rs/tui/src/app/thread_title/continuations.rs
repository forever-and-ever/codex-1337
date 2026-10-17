//! Generated-title allocation after a background name check.

use super::*;
use crate::app_server_session::typed_requests::SessionRpc;

impl App {
    pub(in crate::app) fn apply_generated_thread_title(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        title: String,
    ) {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        use codex_app_server_protocol as api;
        self.request_server(
            server,
            UiRequestScope::Thread(thread_id),
            UiRequestKind::Mutation,
            "Applying generated thread title",
            move |handle| async move {
                let response = handle
                    .thread_read(
                        api::RequestId::String(uuid::Uuid::new_v4().to_string()),
                        api::ThreadReadParams {
                            thread_id: thread_id.to_string(),
                            include_turns: false,
                        },
                    )
                    .await?;
                if response.thread.name.is_some() {
                    return Ok(None);
                }
                handle
                    .thread_allocate_name(
                        api::RequestId::String(uuid::Uuid::new_v4().to_string()),
                        api::ThreadSetNameParams {
                            thread_id: thread_id.to_string(),
                            name: title,
                            naming_mode: Some(api::ThreadNameMode::Generated),
                            conflict_thread_ids: None,
                        },
                    )
                    .await
                    .map(|response| response.thread_name)
            },
            move |app, _, result| {
                match result {
                    Ok(Some(title)) => app
                        .chat_widget
                        .on_thread_name_updated(thread_id, Some(title)),
                    Ok(None) => {}
                    Err(error) => tracing::debug!(%error, "failed to apply generated thread title"),
                }
                Ok(())
            },
        );
    }
}
