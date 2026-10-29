//! Underline colors, which ratatui cells carry and only some terminals draw.

use std::io;
use std::sync::LazyLock;

use codex_terminal_detection::TerminalName;
use codex_terminal_detection::terminal_info;
use crossterm::queue;
use crossterm::style::Print;
use ratatui::style::Color;

/// Whether the terminal draws underline colors, as [`TerminalName::Ghostty`],
/// [`TerminalName::Kitty`] and [`TerminalName::WezTerm`] do unless a multiplexer stands between,
/// which may not pass SGR 58 on.
static DRAWN: LazyLock<bool> = LazyLock::new(|| {
    let terminal = terminal_info();
    terminal.multiplexer.is_none()
        && matches!(
            terminal.name,
            TerminalName::Ghostty | TerminalName::Kitty | TerminalName::WezTerm
        )
});

/// The underline color one draw has set, which an attribute reset clears.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct UnderlineColor(Color);

impl UnderlineColor {
    /// Sets the underline color to `color` where it changes and the terminal draws it.
    pub(super) fn set(&mut self, writer: &mut impl io::Write, color: Color) -> io::Result<()> {
        if self.0 == color || !*DRAWN {
            return Ok(());
        }
        self.0 = color;
        queue!(writer, Print(format!("\x1b[{}m", sgr(color))))
    }
}

/// The SGR parameters setting `color` as the underline color, in the colon form that a terminal
/// parsing subparameters skips whole when it lacks underline colors.
fn sgr(color: Color) -> String {
    let index = match color {
        Color::Reset => return "59".to_owned(),
        Color::Rgb(r, g, b) => return format!("58:2::{r}:{g}:{b}"),
        Color::Indexed(index) => index,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
    };
    format!("58:5:{index}")
}
