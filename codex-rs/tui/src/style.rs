//! Shared TUI colors and semantic styles, including the picker and transcript accent.
//! Informative accents meet minimum contrast on known backgrounds and supported palettes.

mod contrast;
mod history_prompt;

pub(crate) use history_prompt::history_prompt_style;

use crate::color::blend;
use crate::color::is_light;
use crate::terminal_palette::StdoutColorLevel;
use crate::terminal_palette::best_color;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::default_fg;
use crate::terminal_palette::effective_stdout_color_level;
use crate::terminal_palette::rgb_color;
use crate::terminal_palette::stdout_color_level;
use crate::warpaint::Figure;
use crate::warpaint::Ground;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;

const LIGHT_BG_ACCENT_RGB: (u8, u8, u8) = (28, 100, 200);

/// ChatGPT Blue 100 (#A4CDFB), used for selection fills on light backgrounds.
pub(crate) const CHATGPT_BLUE_100: (u8, u8, u8) = (164, 205, 251);

/// ChatGPT Blue 200 (#63A8F8).
pub(crate) const CHATGPT_BLUE_200: (u8, u8, u8) = (99, 168, 248);

/// Shared accent for picker selection backgrounds and transcript foreground emphasis.
pub(crate) const UI_ACCENT: (u8, u8, u8) = CHATGPT_BLUE_200;

/// The result marker inherits the row fill, including the reverse-video selection fallback.
pub(crate) fn task_marker(unread: bool, row: Style) -> ratatui::text::Span<'static> {
    let (symbol, rgb, ansi) = if unread {
        ("●", (0x3b, 0x82, 0xf6), Color::Blue)
    } else {
        ("○", (128, 128, 128), Color::DarkGray)
    };
    let color = match effective_stdout_color_level() {
        StdoutColorLevel::Ansi16 => ansi,
        StdoutColorLevel::Ansi256 => best_color(rgb),
        StdoutColorLevel::TrueColor | StdoutColorLevel::Unknown => rgb_color(rgb),
    };
    // TELOS(warpaint): task state uses pack accents, including reverse-video rows.
    let color = if unread {
        Figure::Accent.repaint(color)
    } else {
        Figure::InkMuted.repaint(color)
    };
    // In reverse video the terminal paints glyphs with the background color.
    let style = if row.add_modifier.contains(Modifier::REVERSED) {
        row.bg(color)
    } else {
        row.fg(color)
    };
    ratatui::text::Span::styled(symbol, style)
}

#[derive(Clone, Copy)]
pub(crate) enum StatusTone {
    Success,
    Attention,
    Failure,
}

/// Semantic status colors that preserve the terminal's configured palette.
pub(crate) fn status_style(tone: StatusTone) -> Style {
    status_style_for(tone, default_bg(), effective_stdout_color_level())
}

fn status_style_for(
    tone: StatusTone,
    terminal_bg: Option<(u8, u8, u8)>,
    color_level: StdoutColorLevel,
) -> Style {
    let light = terminal_bg.is_some_and(is_light);
    let color = match (tone, color_level) {
        (_, StdoutColorLevel::Unknown) => Color::Reset,
        (StatusTone::Success, _) => Color::Green,
        (StatusTone::Failure, _) => Color::Red,
        // Yellow can disappear on light themes; use it only with a known dark background.
        (StatusTone::Attention, _) if light || terminal_bg.is_none() => Color::Reset,
        (StatusTone::Attention, _) => Color::Yellow,
    };
    Style::default().fg(color).bold()
}
// Decorative table rules should remain visible without competing with cell content.
const TABLE_SEPARATOR_FG_ALPHA: f32 = 0.20;

pub fn user_message_style() -> Style {
    user_message_style_for(default_bg())
}

pub(crate) fn formatted_output_style() -> Style {
    let preferred = default_fg().map_or(Color::Cyan, |foreground| {
        rgb_color(blend((90, 165, 180), foreground, /*alpha*/ 0.25))
    });
    Style::default().fg(Figure::Info.repaint(readable_color_on(preferred, /*background*/ None)))
}

/// Blend the floating HUD slightly away from the terminal background in either theme.
const SHORTCUT_HUD_TINT: f32 = 0.052;

pub(crate) fn shortcut_hud_style() -> Style {
    default_bg().map_or_else(Style::default, |bg| {
        let shade = if is_light(bg) { 0 } else { 255 };
        // TELOS(warpaint): a raised shortcut guide wears the active pack.
        Style::default().bg(Ground::Dais.repaint(best_color(blend(
            (shade, shade, shade),
            bg,
            SHORTCUT_HUD_TINT,
        ))))
    })
}

pub fn proposed_plan_style() -> Style {
    proposed_plan_style_for(default_bg())
}

/// Returns a low-contrast rule style for separators within markdown tables.
pub(crate) fn table_separator_style() -> Style {
    table_separator_style_for(default_fg(), default_bg(), stdout_color_level())
}

/// Returns the shared accent style for active or selected TUI controls.
pub(crate) fn accent_style() -> Style {
    if matches!(
        effective_stdout_color_level(),
        StdoutColorLevel::TrueColor | StdoutColorLevel::Ansi256
    ) && let Some(mut style) =
        crate::render::highlight::foreground_style_for_scopes(&["codex.accent"])
    {
        if let Some(Color::Rgb(r, g, b)) = style.fg {
            style = style.fg(best_color((r, g, b)));
        }
        return style.bold();
    }
    accent_style_for(default_bg())
}

/// Returns the foreground accent without imposing bold or dim text modifiers.
pub(crate) fn accent_color() -> Color {
    accent_color_for(default_bg())
}

/// Resolve emphasis against the fill actually painted behind it.
pub(crate) fn accent_color_on(background: Option<Color>) -> Color {
    accent_color_for(background_rgb(background))
}

fn background_rgb(background: Option<Color>) -> Option<(u8, u8, u8)> {
    match background {
        Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
        Some(Color::Indexed(index)) if index >= 16 => {
            Some(crate::terminal_palette::XTERM_COLORS[usize::from(index)])
        }
        None | Some(Color::Reset) => default_bg(),
        _ => None,
    }
}

/// Keep theme-derived text readable on its painted surface, preserving terminal-owned ANSI colors.
pub(crate) fn readable_color_on(preferred: Color, background: Option<Color>) -> Color {
    // TELOS(warpaint): packs prove the ink and every token on the grounds they paint; at an
    // unknown depth upstream paints no computed color, so no token stands in for one.
    if preferred == Color::Reset {
        return preferred;
    }
    if crate::warpaint::is_token(preferred) {
        return if effective_stdout_color_level() == StdoutColorLevel::Unknown {
            Color::Reset
        } else {
            preferred
        };
    }
    let preferred = match preferred {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(index) if index >= 16 => {
            Some(crate::terminal_palette::XTERM_COLORS[usize::from(index)])
        }
        Color::Reset => default_fg(),
        _ => return preferred,
    };
    preferred.map_or(Color::Reset, |preferred| {
        contrast::foreground(
            preferred,
            background_rgb(background),
            effective_stdout_color_level(),
        )
    })
}

/// Secondary text uses a measured foreground instead of terminal-dependent dimming.
pub(crate) fn secondary_text_style() -> Style {
    let preferred = default_fg()
        .zip(default_bg())
        .map_or(Color::Reset, |(fg, bg)| {
            rgb_color(blend(fg, bg, /*alpha*/ 0.6))
        });
    Style::default()
        .fg(Figure::InkMuted.repaint(readable_color_on(preferred, /*background*/ None)))
        .not_dim()
        .not_bold()
}

/// Bring a focusable surface closer to the terminal background while keeping its text readable.
pub(crate) fn quiet_workspace_style() -> Style {
    let base = Style::default().not_bold().not_reversed().not_underlined();
    let Some(bg) = default_bg() else {
        return base.fg(Color::Reset).bg(Color::Reset).dim();
    };
    let fill = best_color(blend(user_message_bg_rgb(bg), bg, /*alpha*/ 0.40));
    let foreground = default_fg().map_or(Color::Reset, |fg| {
        best_color(blend(fg, bg, /*alpha*/ 0.66))
    });
    // TELOS(warpaint): quiet panes retain semantic pack colors, not resolved RGB.
    base.bg(Ground::Prompt.repaint(fill))
        .fg(Figure::InkMuted.repaint(readable_color_on(foreground, Some(fill))))
}

pub(crate) fn selection_style() -> Style {
    let style = contrast::selection_style(default_bg(), effective_stdout_color_level());
    style
        .fg(Figure::InkSelected.repaint(style.fg.unwrap_or_default()))
        .bg(Ground::Selection.repaint(style.bg.unwrap_or_default()))
}

/// Strengthen a control's fill using the terminal's surface tint and readable foreground.
pub(crate) fn hover_style(style: Style) -> Style {
    let (Some(terminal_bg), Some(surface)) = (default_bg(), background_rgb(style.bg)) else {
        return style.underlined();
    };
    let fill = best_color(shade_rgb(shade_rgb(surface, terminal_bg), terminal_bg));
    if fill == Color::Reset || Some(fill) == style.bg {
        return style.underlined();
    }
    // TELOS(warpaint): a hovered control uses the pack's proven highlighted pair.
    style
        .bg(Ground::Selection.repaint(fill))
        .fg(Figure::InkSelected.repaint(readable_color_on(
            style.fg.unwrap_or(Color::Reset),
            Some(fill),
        )))
}

/// Resolve emphasis on the shaded prompt surface, including its quantized background.
#[allow(dead_code, reason = "Used by later layers of the TUI refresh stack.")]
pub(crate) fn user_message_accent_color() -> Color {
    user_message_accent_color_for(default_bg(), effective_stdout_color_level())
}

#[allow(dead_code, reason = "Used by later layers of the TUI refresh stack.")]
fn user_message_accent_color_for(
    background: Option<(u8, u8, u8)>,
    level: StdoutColorLevel,
) -> Color {
    let preferred = if background.is_some_and(is_light) {
        LIGHT_BG_ACCENT_RGB
    } else {
        UI_ACCENT
    };
    let surface = background.and_then(|background| {
        match crate::terminal_palette::best_color_for_level(user_message_bg_rgb(background), level)
        {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            Color::Indexed(index) => {
                Some(crate::terminal_palette::XTERM_COLORS[usize::from(index)])
            }
            _ => None,
        }
    });
    Figure::Accent.repaint(contrast::foreground(preferred, surface, level))
}

/// Muted amber keeps the passive warning count visible without looking like an error.
pub(crate) fn warning_notice_style() -> Style {
    warning_notice_style_for(default_bg(), effective_stdout_color_level())
}

fn warning_notice_style_for(background: Option<(u8, u8, u8)>, level: StdoutColorLevel) -> Style {
    let preferred = if background.is_some_and(is_light) {
        (139, 98, 20)
    } else {
        (196, 167, 103)
    };
    Style::default()
        .fg(Figure::Caution.repaint(contrast::foreground(preferred, background, level)))
        .remove_modifier(ratatui::style::Modifier::DIM)
}

pub(crate) fn footer_hint_label_style() -> Style {
    secondary_text_style()
}

/// Returns the style for a user-authored message using the provided terminal background.
pub fn user_message_style_for(terminal_bg: Option<(u8, u8, u8)>) -> Style {
    match terminal_bg {
        Some(bg) => Style::default().bg(user_message_bg(bg)),
        None => Style::default(),
    }
}

pub fn proposed_plan_style_for(terminal_bg: Option<(u8, u8, u8)>) -> Style {
    match terminal_bg {
        Some(bg) => Style::default().bg(proposed_plan_bg(bg)),
        None => Style::default(),
    }
}

/// Returns the shared accent style for the provided terminal background.
pub(crate) fn accent_style_for(terminal_bg: Option<(u8, u8, u8)>) -> Style {
    Style::default().fg(accent_color_for(terminal_bg)).bold()
}

fn accent_color_for(terminal_bg: Option<(u8, u8, u8)>) -> Color {
    let preferred = if terminal_bg.is_some_and(is_light) {
        LIGHT_BG_ACCENT_RGB
    } else {
        UI_ACCENT
    };
    Figure::Accent.repaint(contrast::foreground(
        preferred,
        terminal_bg,
        effective_stdout_color_level(),
    ))
}

fn table_separator_style_for(
    terminal_fg: Option<(u8, u8, u8)>,
    terminal_bg: Option<(u8, u8, u8)>,
    color_level: StdoutColorLevel,
) -> Style {
    let (Some(fg), Some(bg)) = (terminal_fg, terminal_bg) else {
        return Style::default().dim();
    };
    let separator_rgb = blend(fg, bg, TABLE_SEPARATOR_FG_ALPHA);
    match color_level {
        StdoutColorLevel::TrueColor => {
            Style::default().fg(Figure::Rule.repaint(rgb_color(separator_rgb)))
        }
        StdoutColorLevel::Ansi256 => {
            Style::default().fg(Figure::Rule.repaint(best_color(separator_rgb)))
        }
        StdoutColorLevel::Ansi16 | StdoutColorLevel::Unknown => Style::default().dim(),
    }
}

#[allow(clippy::disallowed_methods)]
pub fn user_message_bg(terminal_bg: (u8, u8, u8)) -> Color {
    Ground::Prompt.repaint(best_color(user_message_bg_rgb(terminal_bg)))
}

pub(crate) fn user_message_bg_rgb(_terminal_bg: (u8, u8, u8)) -> (u8, u8, u8) {
    // TELOS(warpaint): effects blend toward the fill the pack paints.
    crate::warpaint::ground_rgb(Ground::Prompt)
}

fn shade_rgb(surface: (u8, u8, u8), terminal_bg: (u8, u8, u8)) -> (u8, u8, u8) {
    let (top, alpha) = if is_light(terminal_bg) {
        ((0, 0, 0), 0.04)
    } else {
        ((255, 255, 255), 0.12)
    };
    blend(top, surface, alpha)
}

#[allow(clippy::disallowed_methods)]
pub fn proposed_plan_bg(terminal_bg: (u8, u8, u8)) -> Color {
    Ground::Dais.repaint(user_message_bg(terminal_bg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn status_colors_preserve_light_terminal_themes() {
        for level in [StdoutColorLevel::TrueColor, StdoutColorLevel::Ansi256] {
            for bg in [(255, 255, 255), (130, 130, 130), (220, 210, 180)] {
                for (tone, color) in [
                    (StatusTone::Success, Color::Green),
                    (StatusTone::Attention, Color::Reset),
                    (StatusTone::Failure, Color::Red),
                ] {
                    assert_eq!(
                        status_style_for(tone, Some(bg), level),
                        Style::default().fg(color).bold(),
                    );
                }
            }
            for bg in [(0, 0, 0), (0, 218, 0)] {
                assert_eq!(
                    status_style_for(StatusTone::Attention, Some(bg), level),
                    Style::default().fg(Color::Yellow).bold(),
                );
            }
            assert_eq!(
                status_style_for(StatusTone::Attention, /*terminal_bg*/ None, level),
                Style::default().fg(Color::Reset).bold(),
            );
        }
    }

    #[test]
    fn status_colors_preserve_ansi16_and_no_color_fallbacks() {
        for (tone, light, dark) in [
            (StatusTone::Success, Color::Green, Color::Green),
            (StatusTone::Attention, Color::Reset, Color::Yellow),
            (StatusTone::Failure, Color::Red, Color::Red),
        ] {
            for (bg, expected) in [((255, 255, 255), light), ((0, 0, 0), dark)] {
                assert_eq!(
                    status_style_for(tone, Some(bg), StdoutColorLevel::Ansi16),
                    Style::default().fg(expected).bold()
                );
                assert_eq!(
                    status_style_for(tone, Some(bg), StdoutColorLevel::Unknown),
                    Style::default().fg(Color::Reset).bold()
                );
            }
        }
    }

    #[test]
    fn table_separator_dims_when_palette_aware_color_is_unavailable() {
        let expected = Style::default().dim();

        assert_eq!(
            table_separator_style_for(
                Some((255, 255, 255)),
                Some((0, 0, 0)),
                StdoutColorLevel::Ansi16,
            ),
            expected
        );
        assert_eq!(
            table_separator_style_for(
                /*terminal_fg*/ None,
                Some((0, 0, 0)),
                StdoutColorLevel::TrueColor,
            ),
            expected
        );
    }
}
