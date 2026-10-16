use super::App;
use crate::{app_server_session::AppServerSession, pager_overlay::Overlay, tui::Tui};
use codex_protocol::ThreadId;
impl App {
    pub(super) async fn context_inventory(
        &mut self,
        tui: &mut Tui,
        server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        match crate::ui_latency::rpc(server.thread_context_inventory(thread_id)).await {
            Ok(inventory) => {
                self.overlay = Some(Overlay::new_transcript(
                    vec![std::sync::Arc::new(
                        crate::history_cell::PreambleCell::inventory(inventory),
                    )],
                    self.keymap.pager.clone(),
                    self.local_settings
                        .copy_on_select(&codex_terminal_detection::terminal_info()),
                    self.local_settings.tui.mouse_scroll_speed.unwrap_or(1.0),
                ));
                tui.frame_requester().schedule_frame();
            }
            Err(error) => self
                .chat_widget
                .add_error_message(format!("Context snapshot: {error}")),
        }
    }
}
