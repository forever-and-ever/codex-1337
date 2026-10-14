//! Neon: the header is a sign of pink and cyan tubes; one letter is always on its way out.

use super::kit::CYAN;
use super::kit::Lottery;
use super::kit::PINK;
use super::kit::VIOLET;
use super::kit::WHITE;
use super::kit::chance;
use super::kit::frac;
use super::kit::hash;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Neon",
    blurb: "a neon sign with a failing tube",
    summon: || Box::new(Neon),
};

/// Seconds each faulty letter lasts, and the share of them it spends dark at the end.
const FAULT: f64 = 6.0;
const DARK: f64 = 0.93;
/// A power surge flares the sign, which then ignites again.
const SURGE: Lottery = Lottery {
    every: 15.0,
    odds: 0.33,
    span: 3.0,
    seed: 41,
};

struct Neon;

impl Shimmer for Neon {
    fn status(&self, stage: &Stage, row: &mut Row) {
        let t = stage.t;
        let flicker = (t / 0.064).floor() as i64;
        let fault = frac(t / FAULT);
        let faulty =
            (hash(13 * (t / FAULT).floor() as i64 + 1) * stage.header.len() as f64) as usize;
        let warmth = SURGE.at(t).map_or(stage.header_t, |surge| {
            stage.header_t.min(3.0 * surge - 0.5)
        });
        row.mark = vec![
            Ink::of('◉')
                .fg(if stage.busy() { CYAN } else { VIOLET })
                .bold(true),
        ];
        let mut word = 0;
        row.header = stage.letters(|i, letter| {
            if letter == " " {
                word += 1;
                return Ink::default();
            }
            let tube = match (stage.busy(), word % 2) {
                (false, _) => VIOLET,
                (true, 0) => PINK,
                (true, _) => CYAN,
            };
            let index = i as i64;
            if warmth < 0.0 {
                return Ink::default()
                    .fg(WHITE.blend(tube, /*alpha*/ 0.5))
                    .bg(tube.blend(stage.bg, /*alpha*/ 0.3))
                    .bold(true);
            }
            let ignites = 0.06 * i as f64 + 0.1 * hash(7 * stage.headers + index);
            let lit = warmth > ignites + 0.25
                || warmth > ignites && chance(13 * index + flicker, /*odds*/ 0.5);
            let failing = i == faulty
                && (fault >= DARK || chance(7 * index + 3 * flicker, 0.8 * fault * fault));
            if !lit || failing {
                return Ink::default().fg(tube.blend(stage.bg, /*alpha*/ 0.22));
            }
            if stage.busy() {
                Ink::default()
                    .fg(WHITE.blend(tube, /*alpha*/ 0.2))
                    .bg(tube.blend(stage.bg, /*alpha*/ 0.2))
                    .bold(true)
            } else {
                Ink::default()
                    .fg(tube.blend(stage.bg, /*alpha*/ 0.7))
                    .bg(tube.blend(stage.bg, /*alpha*/ 0.08))
            }
        });
    }

    fn tool(&self, stage: &Stage) -> Ink {
        Ink::of('◦').fg(PINK.blend(stage.bg, /*alpha*/ 0.6))
    }
}
