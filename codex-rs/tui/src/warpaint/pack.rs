//! Packs: the palettes warpaint paints with. `scripts/warpaint` smelts each pack from a spec,
//! proves every pair the vocabulary declares, and writes it here; the TUI derives no color.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use ratatui::style::Modifier;
use serde::Deserialize;
use serde::Deserializer;

use super::paint::Brackets;
use super::paint::Figures;
use super::paint::Grounds;
use super::paint::HEAT_STEPS;
use super::paint::Paint;
use super::paint::Syntax;
use super::paint::Syntaxes;

/// An sRGB color, spelled `#rrggbb` in pack files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Rgb(pub(crate) u8, pub(crate) u8, pub(crate) u8);

impl Rgb {
    pub(crate) fn tuple(self) -> (u8, u8, u8) {
        (self.0, self.1, self.2)
    }
}

#[derive(Debug)]
pub(crate) struct BadRgb(String);

impl fmt::Display for BadRgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not a #rrggbb color", self.0)
    }
}

impl FromStr for Rgb {
    type Err = BadRgb;

    fn from_str(text: &str) -> Result<Self, BadRgb> {
        let bad = || BadRgb(text.to_owned());
        let digits = text
            .strip_prefix('#')
            .filter(|digits| digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(bad)?;
        let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).map_err(|_| bad());
        Ok(Self(channel(0)?, channel(2)?, channel(4)?))
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

pub(crate) struct Pack {
    pub(crate) name: &'static str,
    pub(crate) ground: Grounds,
    pub(crate) figure: Figures,
    pub(crate) syntax: Syntaxes,
    pub(crate) bracket: Brackets,
    /// Thread identities, in slot order; never empty.
    pub(crate) livery: Box<[Rgb]>,
    pub(crate) heat: [Rgb; HEAT_STEPS],
    /// Tool output's sixteen colors, in xterm order.
    pub(crate) terminal: [Rgb; 16],
    /// The modifiers syntax classes carry wherever they are bound, as `[style]` gives them.
    pub(crate) style: Box<[(Syntax, Modifier)]>,
}

/// A modifier a pack may give a syntax class.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Emphasis {
    Bold,
    Italic,
}

impl From<Emphasis> for Modifier {
    fn from(emphasis: Emphasis) -> Self {
        match emphasis {
            Emphasis::Bold => Self::BOLD,
            Emphasis::Italic => Self::ITALIC,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackFile {
    livery: Vec<Rgb>,
    heat: [Rgb; HEAT_STEPS],
    terminal: [Rgb; 16],
    ground: Grounds,
    figure: Figures,
    syntax: Syntaxes,
    bracket: Brackets,
    #[serde(default)]
    style: HashMap<Syntax, Vec<Emphasis>>,
}

#[derive(Debug)]
pub(crate) enum PackError {
    Toml(toml::de::Error),
    NoLivery,
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(err) => err.fmt(f),
            Self::NoLivery => f.write_str("livery is empty"),
        }
    }
}

impl Pack {
    /// The pack's color for a token.
    pub(crate) fn rgb(&self, paint: Paint) -> Rgb {
        match paint {
            Paint::Ground(ground) => self.ground[ground],
            Paint::Figure(figure) => self.figure[figure],
            Paint::Syntax(syntax) => self.syntax[syntax],
            Paint::Bracket(bracket) => self.bracket[bracket],
            Paint::Heat(step) => self.heat[step],
            Paint::Livery(slot) => self.livery[slot % self.livery.len()],
        }
    }

    fn parse(name: &'static str, source: &str) -> Result<Self, PackError> {
        let file: PackFile = toml::from_str(source).map_err(PackError::Toml)?;
        if file.livery.is_empty() {
            return Err(PackError::NoLivery);
        }
        Ok(Self {
            name,
            ground: file.ground,
            figure: file.figure,
            syntax: file.syntax,
            bracket: file.bracket,
            livery: file.livery.into_boxed_slice(),
            heat: file.heat,
            terminal: file.terminal,
            style: file
                .style
                .into_iter()
                .map(|(syntax, emphases)| {
                    let modifier = emphases.into_iter().map(Modifier::from).collect();
                    (syntax, modifier)
                })
                .collect(),
        })
    }
}

pub(crate) const DEFAULT: &str = "slag";

/// The default pack's place in [`EMBEDDED`], and so in [`packs`]. It is found at compile time:
/// a default that names no embedded pack fails the build.
pub(super) const DEFAULT_INDEX: usize = {
    let mut index = 0;
    while index < EMBEDDED.len() && !same(EMBEDDED[index].0, DEFAULT) {
        index += 1;
    }
    assert!(index < EMBEDDED.len(), "the default pack is embedded");
    index
};

/// Whether two names are the same, at compile time.
const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut at = 0;
    while at < a.len() {
        if a[at] != b[at] {
            return false;
        }
        at += 1;
    }
    true
}

/// Every pack in `packs/`, sorted by name.
const EMBEDDED: &[(&str, &str)] = &[
    ("crucible", include_str!("packs/crucible.toml")),
    ("eclipse", include_str!("packs/eclipse.toml")),
    ("magnetar", include_str!("packs/magnetar.toml")),
    ("prominence", include_str!("packs/prominence.toml")),
    ("shadow-play", include_str!("packs/shadow-play.toml")),
    ("slag", include_str!("packs/slag.toml")),
    ("sodium", include_str!("packs/sodium.toml")),
    ("uranium", include_str!("packs/uranium.toml")),
];

/// The embedded packs in [`EMBEDDED`]'s order, parsed once. They are generated and checked
/// artifacts, so a pack that fails to parse is a build defect: the TUI refuses it at first use,
/// on startup, rather than wear a pack nobody chose.
#[expect(
    clippy::panic,
    reason = "an embedded pack that fails to parse is a build defect, which embedded_packs_are_the_pack_directory refuses"
)]
pub(crate) fn packs() -> &'static [Pack] {
    static PACKS: OnceLock<Box<[Pack]>> = OnceLock::new();
    PACKS.get_or_init(|| {
        EMBEDDED
            .iter()
            .map(|&(name, source)| {
                Pack::parse(name, source)
                    .unwrap_or_else(|err| panic!("embedded pack {name} is invalid: {err}"))
            })
            .collect()
    })
}

/// Where in [`packs`] the pack called `name` is.
pub(crate) fn find(name: &str) -> Option<usize> {
    packs().iter().position(|pack| pack.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn embedded_packs_are_the_pack_directory() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/warpaint/packs");
        let mut files: Vec<String> = std::fs::read_dir(directory)
            .expect("read packs/")
            .map(|entry| entry.expect("pack entry").path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "toml")
            })
            .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
            .collect();
        files.sort();
        let embedded: Vec<&str> = EMBEDDED.iter().map(|&(name, _)| name).collect();
        assert_eq!(embedded, files);
        assert_eq!(packs().len(), EMBEDDED.len(), "every embedded pack parses");
    }
}
