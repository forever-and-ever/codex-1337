//! Submitted prompts retain their pack-owned ground even without terminal color reports.

use crate::warpaint::Ground;
use ratatui::style::Style;

pub(crate) fn history_prompt_style() -> Style {
    Style::default().bg(Ground::Prompt.color())
}
