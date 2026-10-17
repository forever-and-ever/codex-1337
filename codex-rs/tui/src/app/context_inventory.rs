use super::App;
use super::turn_submissions::ui_requests::UiRequestKind;
use super::turn_submissions::ui_requests::UiRequestScope;
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol as api;
use codex_protocol::ThreadId;

use crate::pager_overlay::Overlay;
use crate::tui::Tui;
impl App {
    pub(super) async fn context_inventory(
        &mut self,
        tui: &mut Tui,
        server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        let frame = tui.frame_requester();
        self.request_server(
            server,
            UiRequestScope::DisplayedThread(thread_id),
            UiRequestKind::ReadOnly,
            "Loading context snapshot",
            move |handle| async move {
                handle
                    .request_typed::<api::ThreadContextInventoryResponse>(
                        api::ClientRequest::ThreadContextInventory {
                            request_id: api::RequestId::String(uuid::Uuid::new_v4().to_string()),
                            params: api::ThreadContextInventoryParams {
                                thread_id: thread_id.to_string(),
                            },
                        },
                    )
                    .await
                    .map(|response| response.inventory)
            },
            move |app, _, result| {
                match result {
                    Ok(inventory) => {
                        app.overlay = Some(Overlay::new_transcript(
                            vec![std::sync::Arc::new(
                                crate::history_cell::PreambleCell::inventory(inventory),
                            )],
                            app.keymap.pager.clone(),
                            app.local_settings
                                .copy_on_select(&codex_terminal_detection::terminal_info()),
                            app.local_settings.tui.mouse_scroll_speed.unwrap_or(1.0),
                        ));
                        frame.schedule_frame();
                    }
                    Err(error) => app
                        .chat_widget
                        .add_error_message(format!("Context snapshot: {error}")),
                }
                Ok(())
            },
        );
    }
}
