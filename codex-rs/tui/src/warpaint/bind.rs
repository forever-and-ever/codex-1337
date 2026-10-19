//! The binder: where every color in a frame becomes what the terminal is sent, as the frame is
//! flushed. Tokens resolve against the pack worn; upstream's named chrome colors resolve through
//! the roles they stand for; tool output, quarantined at ingestion into indices below 16,
//! resolves through the pack's terminal ramp. At sixteen colors the terminal's palette is the
//! user's: only tokens are translated, to their nearest ANSI meaning.

use std::collections::HashMap;

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;

use super::pack::Pack;
use super::pack::Rgb;
use super::paint::Figure;
use super::paint::Ground;
use super::paint::HEAT_STEPS;
use super::paint::Paint;
use super::paint::Syntax;
use crate::color::perceptual_distance;
use crate::terminal_palette::StdoutColorLevel;
use crate::terminal_palette::indexed_color;
use crate::terminal_palette::rgb_color;
use crate::terminal_palette::xterm_fixed_colors;

/// What the terminal can show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Depth {
    TrueColor,
    /// Packs are shown at the nearest fixed xterm color.
    Ansi256,
    /// The terminal's own palette, or none: packs are not shown.
    Ansi16,
}

impl Depth {
    pub(super) fn current() -> Self {
        match crate::terminal_palette::effective_stdout_color_level() {
            StdoutColorLevel::TrueColor => Self::TrueColor,
            StdoutColorLevel::Ansi256 => Self::Ansi256,
            StdoutColorLevel::Ansi16 | StdoutColorLevel::Unknown => Self::Ansi16,
        }
    }

    pub(super) fn shows_packs(self) -> bool {
        self != Self::Ansi16
    }
}

/// Where the default colors come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Canvas {
    /// The terminal's default colors are the pack's ink and canvas, so `Reset` shows them.
    Invested,
    /// The terminal's default colors are its own, so the binder paints ink and canvas.
    Painted,
}

/// Upstream's named colors, by xterm index, as the figures they stand for in chrome; `None`
/// takes the terminal ramp's color.
const CHROME_FIGURES: [Option<Figure>; 16] = [
    None,
    Some(Figure::Failure),
    Some(Figure::Success),
    Some(Figure::Caution),
    Some(Figure::Link),
    Some(Figure::Accent),
    Some(Figure::Info),
    Some(Figure::InkMuted),
    Some(Figure::InkFaint),
    Some(Figure::Failure),
    Some(Figure::Success),
    Some(Figure::Caution),
    Some(Figure::Link),
    Some(Figure::Accent),
    Some(Figure::Info),
    Some(Figure::Ink),
];
/// Upstream's only named chrome background is black: the recessed board of the split-flap.
const CHROME_GROUNDS: [Option<Ground>; 16] = {
    let mut grounds = [None; 16];
    grounds[0] = Some(Ground::Trough);
    grounds
};
const LIVERY_ANSI: [Color; 6] = [
    Color::Cyan,
    Color::Magenta,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Red,
];

#[derive(Clone, Copy)]
struct Bound {
    color: Color,
    modifier: Modifier,
}

impl Bound {
    const fn plain(color: Color) -> Self {
        Self {
            color,
            modifier: Modifier::empty(),
        }
    }
}

/// A pack resolved for one depth and canvas: binding a cell is a few table lookups.
pub(super) struct Binder {
    depth: Depth,
    /// Foregrounds and backgrounds by `Color::Indexed` value.
    fg: [Bound; 256],
    bg: [Bound; 256],
    named_fg: [Color; 16],
    named_bg: [Color; 16],
    reset_fg: Color,
    reset_bg: Color,
    /// What dimmed ink becomes where packs are shown: the proven faint ink, not the terminal's
    /// guess at half intensity.
    faint: Option<Color>,
    /// RGB literals already quantized at 256 colors, at most [`QUANTIZED`] of them.
    quantized: HashMap<(u8, u8, u8), Color>,
}

/// How many quantized literals a binder remembers before forgetting them all. Tool output and
/// animated blends bring arbitrarily many over a session; a frame shows far fewer.
const QUANTIZED: usize = 4096;

impl Binder {
    pub(super) fn new(pack: &Pack, depth: Depth, canvas: Canvas) -> Self {
        let mut identity = [Bound::plain(Color::Reset); 256];
        for (bound, index) in identity.iter_mut().zip(0..=u8::MAX) {
            *bound = Bound::plain(indexed_color(index));
        }
        let mut binder = Self {
            depth,
            fg: identity,
            bg: identity,
            named_fg: NAMED,
            named_bg: NAMED,
            reset_fg: Color::Reset,
            reset_bg: Color::Reset,
            faint: None,
            quantized: HashMap::new(),
        };
        let tokens = (0..=u8::MAX).filter_map(|token| Some((token, Paint::from_token(token)?)));
        if !depth.shows_packs() {
            for (token, paint) in tokens {
                let bound = ansi(paint);
                binder.fg[usize::from(token)] = bound;
                binder.bg[usize::from(token)] = bound;
            }
            return binder.styled(pack);
        }
        for (index, &rgb) in pack.terminal.iter().enumerate() {
            let shown = Bound::plain(binder.show(rgb));
            binder.fg[index] = shown;
            binder.bg[index] = shown;
        }
        let invested = canvas == Canvas::Invested;
        for (token, paint) in tokens {
            let shown = Bound::plain(binder.show(pack.rgb(paint)));
            let index = usize::from(token);
            binder.fg[index] = if invested && paint == Paint::Figure(Figure::Ink) {
                Bound::plain(Color::Reset)
            } else {
                shown
            };
            binder.bg[index] = if invested && paint == Paint::Ground(Ground::Canvas) {
                Bound::plain(Color::Reset)
            } else {
                shown
            };
        }
        for index in 0..16 {
            binder.named_fg[index] = CHROME_FIGURES[index]
                .map_or(binder.fg[index], |figure| {
                    binder.fg[usize::from(figure.token())]
                })
                .color;
            binder.named_bg[index] = CHROME_GROUNDS[index]
                .map_or(binder.bg[index], |ground| {
                    binder.bg[usize::from(ground.token())]
                })
                .color;
        }
        binder.reset_fg = binder.fg[usize::from(Figure::Ink.token())].color;
        binder.reset_bg = binder.bg[usize::from(Ground::Canvas.token())].color;
        binder.faint = Some(binder.fg[usize::from(Figure::InkFaint.token())].color);
        binder.styled(pack)
    }

    /// The binder adding the pack's style to foreground syntax classes, at any depth, over the
    /// modifiers the cell and the depth already carry.
    fn styled(mut self, pack: &Pack) -> Self {
        for &(syntax, modifier) in &pack.style {
            self.fg[usize::from(syntax.token())].modifier |= modifier;
        }
        self
    }

    fn show(&mut self, rgb: Rgb) -> Color {
        self.literal(rgb.tuple())
    }

    /// An RGB literal: exact in truecolor, the nearest fixed xterm color at 256.
    fn literal(&mut self, rgb: (u8, u8, u8)) -> Color {
        match self.depth {
            Depth::TrueColor | Depth::Ansi16 => rgb_color(rgb),
            Depth::Ansi256 => {
                if self.quantized.len() == QUANTIZED && !self.quantized.contains_key(&rgb) {
                    self.quantized.clear();
                }
                *self.quantized.entry(rgb).or_insert_with(|| quantize(rgb))
            }
        }
    }

    fn channel(&mut self, color: Color, modifier: &mut Modifier, background: bool) -> Color {
        let (table, named, reset) = if background {
            (&self.bg, &self.named_bg, self.reset_bg)
        } else {
            (&self.fg, &self.named_fg, self.reset_fg)
        };
        match color {
            Color::Reset => reset,
            Color::Indexed(index) => {
                let bound = table[usize::from(index)];
                *modifier |= bound.modifier;
                bound.color
            }
            Color::Rgb(r, g, b) => self.literal((r, g, b)),
            color => NAMED
                .iter()
                .position(|&name| name == color)
                .map_or(color, |index| named[index]),
        }
    }

    /// Binds one cell's colors and modifiers.
    pub(super) fn bind(
        &mut self,
        fg: Color,
        bg: Color,
        modifier: Modifier,
    ) -> (Color, Color, Modifier) {
        let mut modifier = modifier;
        let dimmed_ink = modifier.contains(Modifier::DIM) && is_ink(fg);
        let fg = self.channel(
            diff_figure(fg, bg),
            &mut modifier,
            /*background*/ false,
        );
        let bg = self.channel(bg, &mut modifier, /*background*/ true);
        match self.faint {
            Some(faint) if dimmed_ink => {
                modifier.remove(Modifier::DIM);
                (faint, bg, modifier)
            }
            _ => (fg, bg, modifier),
        }
    }

    /// Binds a span's style. Unset colors stay unset, so that patching still inherits them,
    /// unless dimmed ink became the faint ink.
    pub(super) fn bind_style(&mut self, style: Style) -> Style {
        let (fg, bg, modifier) = self.bind(
            style.fg.unwrap_or(Color::Reset),
            style.bg.unwrap_or(Color::Reset),
            style.add_modifier,
        );
        let undimmed = style.add_modifier - modifier;
        Style {
            fg: (style.fg.is_some() || !undimmed.is_empty()).then_some(fg),
            bg: style.bg.map(|_| bg),
            add_modifier: modifier,
            sub_modifier: style.sub_modifier | undimmed,
            ..style
        }
    }
}

/// Upstream's named colors in xterm order.
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

/// Whether a foreground is the page's ink, which a terminal's `DIM` would only approximate.
fn is_ink(color: Color) -> bool {
    matches!(color, Color::Reset | Color::White | Color::Gray)
        || color == Figure::Ink.color()
        || color == Figure::InkMuted.color()
}

/// Upstream paints a diff line's text in its sign's named color over the line's ground. There it
/// stands for the diff's own figure, proven on that ground, not the status it resembles.
fn diff_figure(fg: Color, bg: Color) -> Color {
    match fg {
        Color::Green | Color::LightGreen if bg == Ground::Added.color() => Figure::Added.color(),
        Color::Red | Color::LightRed if bg == Ground::Removed.color() => Figure::Removed.color(),
        _ => fg,
    }
}

/// The nearest fixed xterm color; the first sixteen are the terminal's to define.
fn quantize(rgb: (u8, u8, u8)) -> Color {
    let nearest = xterm_fixed_colors()
        .min_by(|(_, a), (_, b)| {
            perceptual_distance(*a, rgb).total_cmp(&perceptual_distance(*b, rgb))
        })
        .and_then(|(index, _)| u8::try_from(index).ok());
    indexed_color(nearest.unwrap_or(16))
}

/// A token at sixteen colors: the ANSI color, or the modifier, upstream would have painted.
fn ansi(paint: Paint) -> Bound {
    let dim = |color| Bound {
        color,
        modifier: Modifier::DIM,
    };
    match paint {
        Paint::Ground(Ground::Selection) => Bound {
            color: Color::Reset,
            modifier: Modifier::REVERSED,
        },
        Paint::Ground(_) => Bound::plain(Color::Reset),
        Paint::Figure(figure) => match figure {
            Figure::Ink | Figure::Heading | Figure::InkSelected => Bound::plain(Color::Reset),
            Figure::InkMuted | Figure::InkFaint | Figure::Rule => dim(Color::Reset),
            Figure::Accent => Bound::plain(Color::Magenta),
            Figure::Link | Figure::Code | Figure::Info => Bound::plain(Color::Cyan),
            Figure::Success | Figure::Added => Bound::plain(Color::Green),
            Figure::Caution => Bound::plain(Color::Yellow),
            Figure::Failure | Figure::Removed => Bound::plain(Color::Red),
        },
        Paint::Syntax(syntax) => match syntax {
            Syntax::Keyword => Bound::plain(Color::Magenta),
            Syntax::Function => Bound::plain(Color::Blue),
            Syntax::Type | Syntax::Attribute => Bound::plain(Color::Cyan),
            Syntax::String => Bound::plain(Color::Green),
            Syntax::Number | Syntax::Constant => Bound::plain(Color::Yellow),
            Syntax::Comment => dim(Color::Reset),
            Syntax::Punctuation | Syntax::Variable => Bound::plain(Color::Reset),
        },
        Paint::Heat(step) if step < HEAT_STEPS / 2 => dim(Color::Magenta),
        Paint::Heat(_) => Bound::plain(Color::Magenta),
        Paint::Livery(slot) => Bound::plain(LIVERY_ANSI[slot % LIVERY_ANSI.len()]),
    }
}

/// A color as the binder resolves it where packs are shown, short of the pack: what a capture
/// names it.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Source {
    /// The page's ink or canvas.
    Reset,
    Paint(Paint),
    /// The pack's terminal ramp: tool output, and the named chrome colors no role stands for.
    Terminal(u8),
    /// A fixed xterm color, which the terminal shows as its own.
    Xterm(u8),
    Rgb(u8, u8, u8),
}

#[cfg(test)]
impl Source {
    /// A cell's foreground, background and modifiers as [`Binder::bind`] resolves them.
    pub(super) fn of_cell(fg: Color, bg: Color, modifier: Modifier) -> (Self, Self, Modifier) {
        let ground = Self::of(bg, CHROME_GROUNDS.map(|ground| ground.map(Paint::Ground)));
        if modifier.contains(Modifier::DIM) && is_ink(fg) {
            let faint = Self::Paint(Paint::Figure(Figure::InkFaint));
            return (faint, ground, modifier.difference(Modifier::DIM));
        }
        let chrome = CHROME_FIGURES.map(|figure| figure.map(Paint::Figure));
        (Self::of(diff_figure(fg, bg), chrome), ground, modifier)
    }

    fn of(color: Color, chrome: [Option<Paint>; 16]) -> Self {
        match color {
            Color::Reset => Self::Reset,
            Color::Indexed(index) => Paint::from_token(index).map_or(
                if index < 16 {
                    Self::Terminal(index)
                } else {
                    Self::Xterm(index)
                },
                Self::Paint,
            ),
            Color::Rgb(r, g, b) => Self::Rgb(r, g, b),
            named => {
                let index = NAMED
                    .into_iter()
                    .zip(0..)
                    .find_map(|(name, index)| (name == named).then_some(index))
                    .expect("every other color is named");
                chrome[usize::from(index)].map_or(Self::Terminal(index), Self::Paint)
            }
        }
    }
}
