use super::App;
use crate::{app_server_session::AppServerSession, pager_overlay::Overlay, tui::Tui};
use codex_protocol::ThreadId;
use ratatui::{style::Stylize, text::Line};
impl App {
    pub(super) async fn context_inventory(
        &mut self,
        tui: &mut Tui,
        server: &mut AppServerSession,
        thread_id: ThreadId,
    ) {
        match server.thread_context_inventory(thread_id).await {
            Ok(inventory) => {
                let total: u64 = inventory.parts.iter().map(|part| part.tokens).sum();
                let mut lines = vec![
                    Line::from(format!("{total} estimated tokens in retained special context").bold()),
                    Line::from("Snapshot only — not an injection. Superseded copies and amendments are included.".dim()),
                    Line::default(),
                ];
                for part in inventory.parts {
                    let label = match part.group {
                        Some(group) => format!("{group}/{}", part.label),
                        None => part.label,
                    };
                    lines.push(Line::from(format!("{:>9}  {label}", part.tokens)));
                }
                lines.push(Line::default());
                lines.push(Line::from("Raw context".bold()));
                lines.extend(
                    crate::history_cell::sanitize_user_text(inventory.raw.as_str().into())
                        .lines()
                        .map(|line| Line::from(line.to_string())),
                );
                self.overlay = Some(Overlay::new_static_with_lines(
                    lines,
                    "Context snapshot".to_string(),
                    self.keymap.pager.clone(),
                ));
                tui.frame_requester().schedule_frame();
            }
            Err(error) => self
                .chat_widget
                .add_error_message(format!("Context snapshot: {error}")),
        }
    }
}
