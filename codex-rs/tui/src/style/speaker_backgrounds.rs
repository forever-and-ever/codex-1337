//! The speakers' bands: each turn is painted on its speaker's ground, wherever the terminal's
//! background is known, as upstream paints its own fills.

use crate::terminal_palette::default_bg;
use crate::warpaint::Ground;
use ratatui::style::Style;

pub(crate) fn history_prompt_style() -> Style {
    band(Ground::Prompt)
}

pub(crate) fn history_response_style() -> Style {
    band(Ground::Response)
}

pub(crate) fn reasoning_style() -> Style {
    band(Ground::Reasoning)
}

pub(crate) fn context_message_style() -> Style {
    band(Ground::Aside)
}

pub(crate) fn subagent_message_style() -> Style {
    band(Ground::Subagent)
}

fn band(ground: Ground) -> Style {
    default_bg().map_or_else(Style::default, |_| Style::default().bg(ground.color()))
}
