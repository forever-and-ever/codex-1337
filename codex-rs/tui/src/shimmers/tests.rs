//! The glare cap: brightness × saturation × spatiotemporal frequency, measured over the live rows.
//!
//! Each cell is seen as its ink mixed over its background in OKLab, so lightness and chroma both count.
//! Flux sums every cell's color change between ticks, per second, and a changed glyph also moves its
//! ink; stripe sums the color steps between neighbouring cells. Header swaps are content, not shimmer.
//! Mean flux over the script stays at or below 7 ΔE·cells/s, its worst second at or below 24, and
//! stripe's worst second at or below 2 ΔE·cells. The design swatches apply the same measure and cap.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use unicode_width::UnicodeWidthStr;

use super::ROSTER;
use super::engine::Engine;
use super::kit::OCTANTS;
use super::kit::QUADRANTS;
use super::picker::feed;
use super::picker::stage_rows;
use super::script::Script;
use super::stage::Palette;
use super::stage::Rgb;
use super::stage::TICK;

const CAP_FLUX: f64 = 7.0;
const CAP_BURST: f64 = 24.0;
const CAP_STRIPE: f64 = 2.0;
/// Seconds of script measured, and the columns of each live row.
const SECONDS: f64 = 120.0;
const WIDTH: u16 = 72;
/// Rows of the preview's buffer holding the tool row and the status row.
const ROWS: [u16; 2] = [0, 3];

type Lab = [f64; 3];

/// One cell as the eye takes it: its glyph and ink coverage, its ink and background, and their mix.
#[derive(Clone)]
struct Seen {
    glyph: String,
    coverage: f64,
    ink: Lab,
    ground: Lab,
    mixed: Lab,
}

#[test]
fn stock_shimmers_stay_under_the_glare_cap_in_steady_geometry() {
    let palette = Palette {
        fg: Rgb(208.0, 208.0, 208.0),
        bg: Rgb(21.0, 21.0, 21.0),
    };
    let script = Script::new();
    let mut oklab = HashMap::new();
    for (recipe, entry) in ROSTER.iter().enumerate() {
        let mut engine = Engine::new(recipe, palette);
        let (mut flux, mut stripe) = (Vec::new(), Vec::new());
        let mut previous: Option<(&str, Vec<Vec<Seen>>)> = None;
        let mut lead = None;
        for tick in 1..=(SECONDS / TICK) as u64 {
            feed(&mut engine, &script, tick);
            let mut buf = Buffer::empty(Rect::new(
                /*x*/ 0, /*y*/ 0, WIDTH, /*height*/ 4,
            ));
            stage_rows(&mut engine, &script, tick, palette, buf.area, &mut buf);
            let (_, header, _) = script.at(tick as f64 * TICK);

            assert_eq!(
                buf[(2, 0)].symbol(),
                "C",
                "{}: a tool's bullet is one cell",
                entry.name
            );
            let timer = (0..WIDTH).find(|&x| buf[(x, 3)].symbol() == "(");
            let beside = timer.map(|x| usize::from(x) - header.width());
            assert!(
                beside.is_some(),
                "{}: the status row keeps its timer",
                entry.name
            );
            assert!(
                lead.is_none_or(|lead| lead == beside),
                "{}: the status row's lead keeps its width",
                entry.name
            );
            lead = Some(beside);

            let rows: Vec<Vec<Seen>> = ROWS
                .iter()
                .map(|&y| {
                    (0..WIDTH)
                        .map(|x| see(&buf, x, y, palette, &mut oklab))
                        .collect()
                })
                .collect();
            if let Some((was, before)) = &previous
                && *was == header
            {
                let mut moved = 0.0;
                let mut steps = 0.0;
                for (row, before) in rows.iter().zip(before) {
                    for (x, (now, then)) in row.iter().zip(before).enumerate() {
                        moved += distance(then.mixed, now.mixed);
                        let both_shades =
                            shade(&then.glyph).is_some() && shade(&now.glyph).is_some();
                        if then.glyph != now.glyph && !both_shades {
                            moved +=
                                then.coverage.min(now.coverage) * distance(now.ink, now.ground);
                        }
                        if let Some(left) = x.checked_sub(1).map(|left| &row[left]) {
                            steps += distance(left.ground, now.ground)
                                + left.coverage.min(now.coverage) * distance(left.ink, now.ink);
                        }
                    }
                }
                flux.push(moved / TICK);
                stripe.push(steps);
            }
            previous = Some((header, rows));
        }
        let mean = flux.iter().sum::<f64>() / flux.len() as f64;
        let burst = worst_second(&flux);
        let stripes = worst_second(&stripe);
        assert!(
            mean <= CAP_FLUX && burst <= CAP_BURST && stripes <= CAP_STRIPE,
            "{} glares: flux {mean:.2} (cap {CAP_FLUX}), worst second {burst:.1} (cap {CAP_BURST}), stripe {stripes:.2} (cap {CAP_STRIPE})",
            entry.name
        );
    }
}

/// The worst one-second mean of a per-tick series.
fn worst_second(series: &[f64]) -> f64 {
    let window = (1.0 / TICK).round() as usize;
    series
        .windows(window)
        .map(|second| second.iter().sum::<f64>() / window as f64)
        .fold(0.0, f64::max)
}

fn see(buf: &Buffer, x: u16, y: u16, palette: Palette, oklab: &mut HashMap<[u8; 3], Lab>) -> Seen {
    let cell = &buf[(x, y)];
    let rgb = |color: Color, default: Rgb| match color {
        Color::Rgb(r, g, b) => Rgb::from((r, g, b)),
        _ => default,
    };
    let ground = rgb(cell.bg, palette.bg);
    let mut ink = rgb(cell.fg, palette.fg);
    if cell.modifier.contains(Modifier::DIM) {
        ink = ink.blend(ground, /*alpha*/ 0.5);
    }
    let glyph = cell.symbol().to_owned();
    let bold = if cell.modifier.contains(Modifier::BOLD) {
        1.25
    } else {
        1.0
    };
    let coverage = (cover(&glyph) * bold).min(1.0);
    let mut lab = |color: Rgb| {
        let key = [color.0 as u8, color.1 as u8, color.2 as u8];
        *oklab.entry(key).or_insert_with(|| to_oklab(key))
    };
    let (ink, ground) = (lab(ink), lab(ground));
    let mixed = std::array::from_fn(|k| ground[k] + coverage * (ink[k] - ground[k]));
    Seen {
        glyph,
        coverage,
        ink,
        ground,
        mixed,
    }
}

fn shade(glyph: &str) -> Option<f64> {
    match glyph {
        "░" => Some(0.25),
        "▒" => Some(0.5),
        "▓" => Some(0.75),
        _ => None,
    }
}

/// Rough ink coverage of a glyph within its cell.
fn cover(glyph: &str) -> f64 {
    let known = match glyph {
        " " => Some(0.0),
        "·" => Some(0.08),
        "∙" => Some(0.14),
        "•" => Some(0.25),
        "●" => Some(0.55),
        "○" => Some(0.3),
        "◦" => Some(0.18),
        "─" => Some(0.12),
        "━" => Some(0.25),
        "*" => Some(0.2),
        _ => shade(glyph),
    };
    if let Some(known) = known {
        return known;
    }
    let mut chars = glyph.chars();
    let (Some(glyph), None) = (chars.next(), chars.next()) else {
        return 0.4;
    };
    let ones = |bits: usize| f64::from(bits.count_ones());
    if ('\u{2800}'..'\u{2900}').contains(&glyph) {
        0.45 * ones(glyph as usize - 0x2800) / 8.0
    } else if let Some(eighths) = "▁▂▃▄▅▆▇█".chars().position(|block| block == glyph)
    {
        (eighths + 1) as f64 / 8.0
    } else if let Some(bits) = QUADRANTS.iter().position(|&quadrant| quadrant == glyph) {
        ones(bits) / 4.0
    } else if let Some(bits) = OCTANTS.iter().position(|&octant| octant == glyph) {
        ones(bits) / 8.0
    } else {
        0.4
    }
}

fn distance(a: Lab, b: Lab) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn to_oklab([r, g, b]: [u8; 3]) -> Lab {
    let linear = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (linear(r), linear(g), linear(b));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
    ]
}
