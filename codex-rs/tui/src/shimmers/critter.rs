//! Critter: a three-cell face at the bullet that reacts to the token stream and dozes while waiting.

use super::kit::AZURE;
use super::kit::CORAL;
use super::kit::CYAN;
use super::kit::LIME;
use super::kit::Lottery;
use super::kit::MINT;
use super::kit::PINK;
use super::kit::STEEL;
use super::kit::SUN;
use super::kit::VIOLET;
use super::kit::chance;
use super::kit::frac;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Rgb;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;
use super::stage::TICK;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Critter",
    blurb: "a small face reacts to the output",
    summon: || Box::<Critter>::default(),
};

const SNEEZE: Lottery = Lottery {
    every: 13.0,
    odds: 0.15,
    span: 0.9,
    seed: 5,
};
const WINK: Lottery = Lottery {
    every: 7.0,
    odds: 0.3,
    span: 0.5,
    seed: 3,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mood {
    Happy,
    Content,
    Surprised,
    Alarmed,
    Pondering,
    Wink,
    Sneeze,
    Sleepy,
    Asleep,
}

impl Mood {
    fn face(self) -> (&'static str, Rgb) {
        match self {
            Mood::Happy => ("◕‿◕", SUN),
            Mood::Content => ("–‿–", LIME),
            Mood::Surprised => ("◕o◕", CYAN),
            Mood::Alarmed => ("⊙ο⊙", CORAL),
            Mood::Pondering => ("◔_◔", AZURE),
            Mood::Wink => ("◕‿-", PINK),
            Mood::Sneeze => (">ω<", MINT),
            Mood::Sleepy => ("˘‿˘", VIOLET),
            Mood::Asleep => ("–_–", STEEL),
        }
    }
}

#[derive(Default)]
struct Critter {
    /// Happy above 45 tokens a second until the rate falls below 30.
    happy: bool,
    /// The header's tint, crossfading toward the mood's.
    tint: Option<Rgb>,
}

impl Critter {
    fn mood(&self, stage: &Stage) -> Mood {
        if !stage.busy() {
            return if stage.spell_t < 3.0 {
                Mood::Sleepy
            } else {
                Mood::Asleep
            };
        }
        if let Some(sneeze) = SNEEZE.at(stage.t) {
            return if sneeze < 0.45 {
                Mood::Surprised
            } else {
                Mood::Sneeze
            };
        }
        let now = stage.flow(/*window*/ 0.3);
        let then = (stage.tokens_in(/*window*/ 0.6) - stage.tokens_in(/*window*/ 0.3)) / 0.3;
        if stage.quiet > 4.0 {
            Mood::Pondering
        } else if stage.quiet > 1.2 {
            Mood::Alarmed
        } else if now - then > 35.0 {
            Mood::Surprised
        } else if WINK.at(stage.t).is_some() {
            Mood::Wink
        } else if self.happy {
            Mood::Happy
        } else {
            Mood::Content
        }
    }
}

impl Shimmer for Critter {
    fn tick(&mut self, stage: &Stage) {
        let now = stage.flow(/*window*/ 0.3);
        self.happy = now > 45.0 || self.happy && now >= 30.0;
        let target = if stage.busy() {
            self.mood(stage).face().1.blend(stage.fg, /*alpha*/ 0.4)
        } else {
            stage.grey(/*alpha*/ 0.6)
        };
        let tint = self.tint.unwrap_or(stage.fg);
        self.tint = Some(tint.blend(target, (-TICK / 0.25).exp()));
    }

    fn status(&self, stage: &Stage, row: &mut Row) {
        let mood = self.mood(stage);
        let (face, tone) = face_now(mood, stage.t);
        row.mark = face
            .chars()
            .map(|glyph| Ink::of(glyph).fg(tone).bold(true))
            .collect();
        let tint = self.tint.unwrap_or(stage.fg);
        let jostle = (stage.t / 0.1).floor() as i64;
        row.header = stage.letters(|i, _| {
            // A sneeze jostles letters into their neighbors' cells.
            let shoved = (mood == Mood::Sneeze && chance(7 * i as i64 + jostle, /*odds*/ 0.3))
                .then(|| stage.header.get(i + 1))
                .flatten()
                .and_then(|next| {
                    let mut chars = next.chars();
                    chars.next().filter(|_| chars.next().is_none())
                });
            Ink {
                glyph: shoved,
                ..Ink::default().fg(tint)
            }
        });
    }

    fn tool(&self, stage: &Stage) -> Ink {
        if stage.timer % 60 < 2 {
            Ink::of('♪').fg(SUN).bold(true)
        } else if self.mood(stage) == Mood::Asleep {
            Ink::of(if (stage.t / 0.7).floor() as i64 % 2 == 1 {
                'Z'
            } else {
                'z'
            })
            .fg(VIOLET)
        } else {
            Ink::of('·').fg(stage.grey(/*alpha*/ 0.4))
        }
    }
}

/// The face to show: a happy critter blinks every 3.7 seconds.
fn face_now(mood: Mood, t: f64) -> (&'static str, Rgb) {
    let (face, tone) = mood.face();
    if mood == Mood::Happy && frac(t / 3.7) < 0.035 {
        ("–‿–", tone)
    } else {
        (face, tone)
    }
}
