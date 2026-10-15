use super::HistoryCell;
use codex_protocol::tool_input::ToolInput;
use ratatui::text::Line;

#[derive(Debug)]
pub(crate) struct ToolInputCell(pub(crate) ToolInput);

impl HistoryCell for ToolInputCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        Vec::new()
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        Vec::new()
    }

    fn outer_call_id(&self) -> Option<&str> {
        Some(&self.0.call_id)
    }
}
