//! Mouse navigation reuses the same thread-switch transaction as the keyboard.
use super::*;
use crate::subagent_roster::MouseAction;
use crossterm::event::MouseEvent;

impl App {
    pub(super) async fn handle_subagent_mouse(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mouse: MouseEvent,
    ) -> Result<()> {
        if self.overlay.is_some()
            || !self.chat_widget.no_modal_or_popup_active()
            || self.reconnect.offline
        {
            self.agent_navigation
                .roster
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .invalidate_pointer();
            tui.set_mouse_capture(false)?;
            return Ok(());
        }
        let action = self
            .agent_navigation
            .roster
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mouse(mouse);
        match action {
            MouseAction::None => return Ok(()),
            MouseAction::Redraw => {}
            MouseAction::Open(id) => {
                self.agent_navigation
                    .roster
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .invalidate_pointer();
                if self
                    .agent_navigation
                    .get(&id)
                    .is_some_and(|entry| !entry.is_closed)
                    && let Err(error) = self.select_agent_thread(tui, server, id).await
                {
                    self.chat_widget
                        .add_error_message(format!("Could not open subagent: {error}"));
                }
            }
        }
        tui.frame_requester().schedule_frame();
        Ok(())
    }
}
