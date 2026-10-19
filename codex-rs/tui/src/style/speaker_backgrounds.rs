use super::aside_rgb;
use crate::color::blend;
use crate::color::is_light;
use crate::terminal_palette::best_color;
use crate::terminal_palette::default_bg;
use ratatui::style::Style;

struct Tint {
    hue: (u8, u8, u8),
    dark: f32,
    dark_lift: f32,
    light: f32,
}

// Calibrated offline against #151515 by contracting the former fills toward the canvas in
// OKLab: blue/green retain 70% of their distance, purple 50%. Keep the hue anchors; reducing
// only their tint would leave the old neutral lift intact. Light-theme fills are unchanged.
pub(crate) fn history_prompt_style() -> Style {
    speaker_style(&Tint {
        hue: (90, 150, 255),
        dark: 0.105,
        dark_lift: 0.108,
        light: 0.08,
    })
}

pub(crate) fn history_response_style() -> Style {
    speaker_style(&Tint {
        hue: (90, 200, 120),
        dark: 0.068,
        dark_lift: 0.108,
        light: 0.05,
    })
}

pub(crate) fn reasoning_style() -> Style {
    speaker_style(&Tint {
        hue: (245, 150, 55),
        dark: 0.10,
        dark_lift: 0.108,
        light: 0.07,
    })
}

pub(crate) fn context_message_style() -> Style {
    speaker_style(&Tint {
        hue: (165, 105, 220),
        dark: 0.043,
        dark_lift: 0.078,
        light: 0.05,
    })
}

pub(crate) fn subagent_message_style() -> Style {
    speaker_style(&Tint {
        hue: (155, 200, 90),
        dark: 0.10,
        dark_lift: 0.16,
        light: 0.05,
    })
}

fn speaker_style(tint: &Tint) -> Style {
    default_bg().map_or_else(Style::default, |background| {
        let (surface, strength) = if is_light(background) {
            (aside_rgb(background), tint.light)
        } else {
            (
                blend((255, 255, 255), background, tint.dark_lift),
                tint.dark,
            )
        };
        Style::default().bg(best_color(blend(tint.hue, surface, strength)))
    })
}
