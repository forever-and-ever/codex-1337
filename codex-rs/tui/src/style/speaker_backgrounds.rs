//! The speakers' grounds: whatever the model sees is painted on the ground of whoever said it,
//! wherever the terminal's background is known, as upstream paints its own fills; the canvas
//! holds only what the client says.

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

pub(crate) fn environment_style() -> Style {
    band(Ground::Environment)
}

pub(crate) fn message_style() -> Style {
    band(Ground::Message)
}

fn band(ground: Ground) -> Style {
    default_bg().map_or_else(Style::default, |_| Style::default().bg(ground.color()))
}
