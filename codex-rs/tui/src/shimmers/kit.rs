//! The shimmer lib: patterns stock shimmers share, kept bit-for-bit with the design swatches.

use super::stage::Rgb;
use super::stage::TICK;

pub(super) const PINK: Rgb = Rgb(255.0, 78.0, 160.0);
pub(super) const TANGERINE: Rgb = Rgb(255.0, 140.0, 50.0);
pub(super) const SUN: Rgb = Rgb(255.0, 214.0, 64.0);
pub(super) const LIME: Rgb = Rgb(170.0, 240.0, 60.0);
pub(super) const MINT: Rgb = Rgb(60.0, 240.0, 170.0);
pub(super) const CYAN: Rgb = Rgb(50.0, 215.0, 255.0);
pub(super) const AZURE: Rgb = Rgb(70.0, 130.0, 255.0);
pub(super) const VIOLET: Rgb = Rgb(165.0, 105.0, 255.0);
pub(super) const MAGENTA: Rgb = Rgb(230.0, 70.0, 230.0);
pub(super) const CORAL: Rgb = Rgb(255.0, 95.0, 95.0);
pub(super) const GOLD: Rgb = Rgb(255.0, 190.0, 40.0);
pub(super) const ICE: Rgb = Rgb(180.0, 230.0, 255.0);
pub(super) const STEEL: Rgb = Rgb(120.0, 140.0, 165.0);
/// A hot white, brighter than most terminals' default foreground.
pub(super) const WHITE: Rgb = Rgb(245.0, 245.0, 245.0);

/// A deterministic draw in [0, 1) for `seed`.
pub(super) fn hash(seed: i64) -> f64 {
    let bytes = seed.to_le_bytes();
    let low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let mut x = (low ^ 0x9e37_79b9).wrapping_mul(0x85eb_ca6b);
    x ^= x >> 13;
    x = x.wrapping_mul(0xc2b2_ae35);
    f64::from(x ^ (x >> 16)) / 4_294_967_296.0
}

pub(super) fn chance(seed: i64, odds: f64) -> bool {
    hash(seed) < odds
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "The hash is in [0, 1); flooring its scaled value deliberately selects a nonnegative sample, clamped to the slice's last index."
)]
pub(super) fn pick<T: Copy>(seed: i64, list: &[T]) -> T {
    list[((hash(seed) * list.len() as f64) as usize).min(list.len() - 1)]
}

/// Every `every` seconds, one draw at `odds`; a won draw lasts `span` seconds.
pub(super) struct Lottery {
    pub(super) every: f64,
    pub(super) odds: f64,
    pub(super) span: f64,
    pub(super) seed: i64,
}

impl Lottery {
    /// Progress through the won draw under way at `t`, if one is.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "A lottery draw is deliberately identified by its whole animation-clock bucket."
    )]
    pub(super) fn at(&self, t: f64) -> Option<f64> {
        let draw = (t / self.every).floor();
        let into = t - draw * self.every;
        (into < self.span && chance(self.seed + 7919 * draw as i64, self.odds))
            .then_some(into / self.span)
    }
}

pub(super) fn frac(x: f64) -> f64 {
    x - x.floor()
}

/// A cosine ease from 0 to 1 over [0, 1].
pub(super) fn ease(x: f64) -> f64 {
    0.5 - 0.5 * (std::f64::consts::PI * x).cos()
}

/// A smoothstep from `from` to `to`.
pub(super) fn smooth(from: f64, to: f64, x: f64) -> f64 {
    let t = ((x - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A raised-cosine bump of half-width `width` around zero.
pub(super) fn bump(distance: f64, width: f64) -> f64 {
    if distance.abs() < width {
        0.5 + 0.5 * (std::f64::consts::PI * distance / width).cos()
    } else {
        0.0
    }
}

/// One tick of a first-order lag from `value` toward `target` with time constant `tau` seconds.
pub(super) fn lag(value: f64, target: f64, tau: f64) -> f64 {
    value + (target - value) * (1.0 - (-TICK / tau).exp())
}

/// The color at `x` in [0, 1] along evenly spaced `stops`.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "A clamped unit coordinate deliberately selects adjacent color stops; the floored nonnegative index is capped below the last stop."
)]
pub(super) fn ramp(stops: &[Rgb], x: f64) -> Rgb {
    let y = x.clamp(0.0, 1.0) * (stops.len() - 1) as f64;
    let i = (y.floor() as usize).min(stops.len() - 2);
    stops[i + 1].blend(stops[i], y - i as f64)
}

/// A color from `hue` in turns, `saturation` and `lightness`.
pub(super) fn hsl(hue: f64, saturation: f64, lightness: f64) -> Rgb {
    let a = saturation * lightness.min(1.0 - lightness);
    let channel = |n: f64| {
        let k = (n + 12.0 * frac(hue)) % 12.0;
        255.0 * (lightness - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0))
    };
    Rgb(channel(0.0), channel(8.0), channel(4.0))
}

/// Quadrant blocks by bits: upper left 8, upper right 4, lower left 2, lower right 1.
pub(super) const QUADRANTS: [char; 16] = [
    ' ', '▗', '▖', '▄', '▝', '▐', '▞', '▟', '▘', '▚', '▌', '▙', '▀', '▜', '▛', '█',
];

/// Octant blocks by bits: pixel `2 * y + x` of a cell's 2×4 grid is bit `2 * y + x`. Pixels are
/// square on a 1:2 cell. Patterns Unicode encodes outside the octant block map to those characters.
pub(super) const OCTANTS: [char; 256] = [
    ' ', '𜺨', '𜺫', '🮂', '𜴀', '▘', '𜴁', '𜴂', '𜴃', '𜴄', '▝', '𜴅', '𜴆', '𜴇', '𜴈', '▀', '𜴉', '𜴊', '𜴋',
    '𜴌', '🯦', '𜴍', '𜴎', '𜴏', '𜴐', '𜴑', '𜴒', '𜴓', '𜴔', '𜴕', '𜴖', '𜴗', '𜴘', '𜴙', '𜴚', '𜴛', '𜴜', '𜴝',
    '𜴞', '𜴟', '🯧', '𜴠', '𜴡', '𜴢', '𜴣', '𜴤', '𜴥', '𜴦', '𜴧', '𜴨', '𜴩', '𜴪', '𜴫', '𜴬', '𜴭', '𜴮', '𜴯',
    '𜴰', '𜴱', '𜴲', '𜴳', '𜴴', '𜴵', '🮅', '𜺣', '𜴶', '𜴷', '𜴸', '𜴹', '𜴺', '𜴻', '𜴼', '𜴽', '𜴾', '𜴿', '𜵀',
    '𜵁', '𜵂', '𜵃', '𜵄', '▖', '𜵅', '𜵆', '𜵇', '𜵈', '▌', '𜵉', '𜵊', '𜵋', '𜵌', '▞', '𜵍', '𜵎', '𜵏', '𜵐',
    '▛', '𜵑', '𜵒', '𜵓', '𜵔', '𜵕', '𜵖', '𜵗', '𜵘', '𜵙', '𜵚', '𜵛', '𜵜', '𜵝', '𜵞', '𜵟', '𜵠', '𜵡', '𜵢',
    '𜵣', '𜵤', '𜵥', '𜵦', '𜵧', '𜵨', '𜵩', '𜵪', '𜵫', '𜵬', '𜵭', '𜵮', '𜵯', '𜵰', '𜺠', '𜵱', '𜵲', '𜵳', '𜵴',
    '𜵵', '𜵶', '𜵷', '𜵸', '𜵹', '𜵺', '𜵻', '𜵼', '𜵽', '𜵾', '𜵿', '𜶀', '𜶁', '𜶂', '𜶃', '𜶄', '𜶅', '𜶆', '𜶇',
    '𜶈', '𜶉', '𜶊', '𜶋', '𜶌', '𜶍', '𜶎', '𜶏', '▗', '𜶐', '𜶑', '𜶒', '𜶓', '▚', '𜶔', '𜶕', '𜶖', '𜶗', '▐',
    '𜶘', '𜶙', '𜶚', '𜶛', '▜', '𜶜', '𜶝', '𜶞', '𜶟', '𜶠', '𜶡', '𜶢', '𜶣', '𜶤', '𜶥', '𜶦', '𜶧', '𜶨', '𜶩',
    '𜶪', '𜶫', '▂', '𜶬', '𜶭', '𜶮', '𜶯', '𜶰', '𜶱', '𜶲', '𜶳', '𜶴', '𜶵', '𜶶', '𜶷', '𜶸', '𜶹', '𜶺', '𜶻',
    '𜶼', '𜶽', '𜶾', '𜶿', '𜷀', '𜷁', '𜷂', '𜷃', '𜷄', '𜷅', '𜷆', '𜷇', '𜷈', '𜷉', '𜷊', '𜷋', '𜷌', '𜷍', '𜷎',
    '𜷏', '𜷐', '𜷑', '𜷒', '𜷓', '𜷔', '𜷕', '𜷖', '𜷗', '𜷘', '𜷙', '𜷚', '▄', '𜷛', '𜷜', '𜷝', '𜷞', '▙', '𜷟',
    '𜷠', '𜷡', '𜷢', '▟', '𜷣', '▆', '𜷤', '𜷥', '█',
];
