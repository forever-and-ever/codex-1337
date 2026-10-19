//! Tool output speaks the terminal's palette, not the TUI's. Palette colors become indices
//! below 16 and extended colors become RGB, so the TUI's frame binder can tell tool output from
//! its own chrome, which paints named colors and reserves the other indices.

use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Text;

pub(crate) fn quarantine(text: &mut Text<'static>) {
    text.style = style(text.style);
    for line in &mut text.lines {
        line.style = style(line.style);
        for span in &mut line.spans {
            span.style = style(span.style);
        }
    }
}

fn style(style: Style) -> Style {
    Style {
        fg: style.fg.map(color),
        bg: style.bg.map(color),
        ..style
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "quarantine exists to turn tool output's colors into explicit indices and RGB"
)]
fn color(color: Color) -> Color {
    const NAMED: [Color; 16] = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    match color {
        Color::Indexed(index @ 16..) => {
            // The xterm 6x6x6 cube from 16, then a 24-step gray ramp from 232.
            const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let offset = usize::from(index - 16);
            if offset < 216 {
                Color::Rgb(
                    LEVELS[offset / 36],
                    LEVELS[offset / 6 % 6],
                    LEVELS[offset % 6],
                )
            } else {
                let gray = 8 + 10 * (index - 232);
                Color::Rgb(gray, gray, gray)
            }
        }
        color => NAMED
            .into_iter()
            .zip(0..)
            .find(|&(named, _)| named == color)
            .map_or(color, |(_, index)| Color::Indexed(index)),
    }
}
