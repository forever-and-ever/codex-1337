//! The local fork service is systemd-managed. Report status without giving the TUI a
//! competing path to replace it.

use super::*;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::wrapping::word_wrap_lines;
use ratatui::buffer::Buffer;
use ratatui::widgets::Paragraph;

struct DaemonMenuHeader(Vec<Line<'static>>);

impl Renderable for DaemonMenuHeader {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Renderable::render(
            &Paragraph::new(word_wrap_lines(&self.0, usize::from(area.width))),
            area,
            buf,
        );
    }

    fn desired_height(&self, width: u16) -> u16 {
        word_wrap_lines(&self.0, usize::from(width)).len() as u16
    }
}

impl App {
    pub(super) fn open_daemon_menu(&mut self) {
        let status = match (
            &self.app_server_target,
            self.chat_widget.remote_connection.as_ref(),
        ) {
            (AppServerTarget::LocalDaemon { .. }, Some(connection)) => {
                format!("Local shared service: {}", connection.version)
            }
            (AppServerTarget::Remote { .. }, Some(connection)) => {
                format!("Remote shared service: {}", connection.version)
            }
            (AppServerTarget::Remote { .. }, None) => {
                "Not connected to the remote shared service.".to_string()
            }
            _ => "Not connected to the local shared service.".to_string(),
        };
        let header = vec![Line::from("Daemon".bold()), Line::from(status.dim())];
        self.chat_widget.show_selection_view(SelectionViewParams {
            header: Box::new(DaemonMenuHeader(header)),
            items: vec![SelectionItem {
                name: "Close".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            ..SelectionViewParams::confirmation()
        });
    }
}
