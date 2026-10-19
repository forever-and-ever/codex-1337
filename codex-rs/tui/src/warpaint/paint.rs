//! The paint vocabulary. A ground is what a cell is painted on; a figure is text or a glyph
//! painted over a ground; a syntax class is a figure only the scope theme names. Each family is
//! declared once, and the declaration emits the enum, the table a pack file must fill, and the
//! family's token range.
//!
//! A token is a `Color::Indexed` value reserved for warpaint: widgets paint tokens, never pack
//! colors, and the binder resolves them as the frame is flushed. Indices below 16 belong to tool
//! output and are never tokens; no other code may paint an indexed color.

use std::ops::Index;

use ratatui::style::Color;
use serde::Deserialize;

use super::pack::Rgb;
use crate::terminal_palette::indexed_color;

macro_rules! family {
    (
        $(#[$meta:meta])*
        $family:ident, table $table:ident, tokens from $base:literal {
            $($(#[$member_meta:meta])* $member:ident => $key:ident,)+
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub(crate) enum $family {
            $($(#[$member_meta])* $member,)+
        }

        impl $family {
            pub(crate) const ALL: &'static [Self] = &[$(Self::$member,)+];
            const TOKENS: u8 = $base;

            /// The member's key in pack files and `roles.json`.
            pub(crate) fn key(self) -> &'static str {
                match self {
                    $(Self::$member => stringify!($key).trim_start_matches("r#"),)+
                }
            }

            pub(crate) const fn token(self) -> u8 {
                Self::TOKENS + self as u8
            }

            fn from_token(token: u8) -> Option<Self> {
                Self::ALL.get(usize::from(token.checked_sub(Self::TOKENS)?)).copied()
            }
        }

        /// A member as pack files spell it: by its key.
        impl<'de> Deserialize<'de> for $family {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let key = String::deserialize(deserializer)?;
                Self::ALL
                    .iter()
                    .copied()
                    .find(|member| member.key() == key)
                    .ok_or_else(|| serde::de::Error::custom(format!("no member is called {key:?}")))
            }
        }

        /// One color per member, as a pack file spells it.
        #[derive(Clone, Copy, Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub(crate) struct $table {
            $($key: Rgb,)+
        }

        impl Index<$family> for $table {
            type Output = Rgb;

            fn index(&self, member: $family) -> &Rgb {
                match member {
                    $($family::$member => &self.$key,)+
                }
            }
        }
    };
}

family! {
    /// What a cell is painted on.
    Ground, table Grounds, tokens from 0x20 {
        /// The page; the terminal's default background while a pack is worn.
        Canvas => canvas,
        /// Recessed: the composer and other inert panels.
        Trough => trough,
        /// Raised: proposed plans and other blocks lifted off the page.
        Dais => dais,
        /// The selected row.
        Selection => selection,
        /// The user's turn.
        Prompt => prompt,
        /// The assistant's turn.
        Response => response,
        /// The assistant's reasoning, disclosed from its response's header.
        Reasoning => reasoning,
        /// Words neither speaker said: client notices, other threads' messages.
        Aside => aside,
        /// A subagent's turn.
        Subagent => subagent,
        /// An added diff line.
        Added => added,
        /// A removed diff line.
        Removed => removed,
    }
}

family! {
    /// Text and glyphs painted over a ground.
    Figure, table Figures, tokens from 0x40 {
        /// Body text; the terminal's default foreground while a pack is worn.
        Ink => ink,
        /// Secondary text: metadata, resolved detail. Upstream's gray.
        InkMuted => ink_muted,
        /// Tertiary text: hints, scaffolding, elision. Upstream's dark gray and dimmed ink.
        InkFaint => ink_faint,
        /// Text on the selection.
        InkSelected => ink_selected,
        /// Separators and table rules.
        Rule => rule,
        /// Agency and focus. Upstream's magenta.
        Accent => accent,
        /// Links and list markers. Upstream's blue.
        Link => link,
        /// Inline code.
        Code => code,
        /// It worked. Upstream's green.
        Success => success,
        /// Needs attention: warnings, limits, pending decisions. Upstream's yellow.
        Caution => caution,
        /// It went wrong. Upstream's red.
        Failure => failure,
        /// Neutral emphasis: progress, modes, hints. Upstream's cyan.
        Info => info,
        /// Added diff text.
        Added => added,
        /// Removed diff text.
        Removed => removed,
    }
}

family! {
    /// `TextMate` token classes; only the scope theme names them, by token.
    Syntax, table Syntaxes, tokens from 0x60 {
        Keyword => keyword,
        Function => function,
        Type => r#type,
        String => string,
        Number => number,
        Comment => comment,
        Punctuation => punctuation,
        Variable => variable,
        Constant => constant,
        Attribute => attribute,
    }
}

impl Ground {
    /// The ground's token as a color, resolved against the pack in force at flush.
    pub(crate) fn color(self) -> Color {
        indexed_color(self.token())
    }

    /// A color upstream computed for this ground, repainted; see [`repainted`].
    pub(crate) fn repaint(self, computed: Color) -> Color {
        repainted(self.color(), computed)
    }
}

impl Figure {
    /// The figure's token as a color, resolved against the pack in force at flush.
    pub(crate) fn color(self) -> Color {
        indexed_color(self.token())
    }

    /// A color upstream computed for this figure, repainted; see [`repainted`].
    pub(crate) fn repaint(self, computed: Color) -> Color {
        repainted(self.color(), computed)
    }
}

/// Where upstream settled on a color of its own (RGB, or a fixed xterm index), the token stands
/// in and the pack decides at flush; where upstream degraded to the terminal's palette (reset or
/// a named color), its degradation stands.
fn repainted(token: Color, computed: Color) -> Color {
    match computed {
        Color::Rgb(..) => token,
        Color::Indexed(index) if index >= 16 => token,
        degraded => degraded,
    }
}

/// Whether `color` is a token, which the pack decides at flush.
pub(crate) fn is_token(color: Color) -> bool {
    matches!(color, Color::Indexed(token) if Paint::from_token(token).is_some())
}

/// Steps of the activity heat ramp, from the faintest trace to full intensity.
pub(crate) const HEAT_STEPS: usize = 4;
const HEAT_TOKENS: [u8; HEAT_STEPS] = consecutive(0x70);
/// Livery slots: thread identities are registered into them in order of appearance.
pub(crate) const LIVERY_SLOTS: usize = 32;
const LIVERY_TOKENS: [u8; LIVERY_SLOTS] = consecutive(0x80);

/// Every token, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Paint {
    Ground(Ground),
    Figure(Figure),
    Syntax(Syntax),
    /// A heat step, `0..HEAT_STEPS`.
    Heat(usize),
    /// A livery slot, `0..LIVERY_SLOTS`.
    Livery(usize),
}

impl Paint {
    pub(crate) fn from_token(token: u8) -> Option<Self> {
        Ground::from_token(token)
            .map(Self::Ground)
            .or_else(|| Figure::from_token(token).map(Self::Figure))
            .or_else(|| Syntax::from_token(token).map(Self::Syntax))
            .or_else(|| HEAT_TOKENS.binary_search(&token).ok().map(Self::Heat))
            .or_else(|| LIVERY_TOKENS.binary_search(&token).ok().map(Self::Livery))
    }
}

/// A heat step's token; `step` counts from the faintest, `0..HEAT_STEPS`.
pub(crate) fn heat(step: usize) -> Color {
    indexed_color(HEAT_TOKENS[step])
}

/// A livery slot's token; slots wrap.
pub(crate) fn livery(slot: usize) -> Color {
    indexed_color(LIVERY_TOKENS[slot % LIVERY_SLOTS])
}

/// `N` consecutive tokens from `first`; a run past the last index fails to compile.
const fn consecutive<const N: usize>(first: u8) -> [u8; N] {
    let mut tokens = [first; N];
    let mut step = 1;
    while step < N {
        tokens[step] = tokens[step - 1] + 1;
        step += 1;
    }
    tokens
}
