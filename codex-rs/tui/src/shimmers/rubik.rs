//! Rubik: two quadrant cells are a two-color puzzle that scrambles and solves itself as tokens arrive.

use super::kit::AZURE;
use super::kit::CORAL;
use super::kit::CYAN;
use super::kit::GOLD;
use super::kit::LIME;
use super::kit::MAGENTA;
use super::kit::MINT;
use super::kit::PINK;
use super::kit::QUADRANTS;
use super::kit::SUN;
use super::kit::TANGERINE;
use super::kit::VIOLET;
use super::kit::WHITE;
use super::kit::chance;
use super::kit::hash;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Rgb;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;
use super::stage::TICK;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Rubik",
    blurb: "a two-cell puzzle solves itself",
    summon: || Box::<Rubik>::default(),
};

/// The five moves as sticker permutations, `after[i] = before[move[i]]`: turn the left cell either
/// way, turn the right cell either way, swap the inner columns.
const TWISTS: [[usize; 8]; 5] = [
    [2, 0, 3, 1, 4, 5, 6, 7],
    [1, 3, 0, 2, 4, 5, 6, 7],
    [0, 1, 2, 3, 6, 4, 7, 5],
    [0, 1, 2, 3, 5, 7, 4, 6],
    [0, 4, 2, 6, 1, 5, 3, 7],
];
const TWIST_SIGNS: [char; 5] = ['↻', '↺', '↻', '↺', '⇄'];
const UNDO: [usize; 5] = [1, 0, 3, 2, 4];
/// A sticker's quadrant bit within its cell, in sticker order.
const QUADRANT_BITS: [usize; 4] = [8, 4, 2, 1];
const SPLITS: [(Rgb, Rgb); 5] = [
    (PINK, CYAN),
    (SUN, VIOLET),
    (LIME, MAGENTA),
    (TANGERINE, AZURE),
    (CORAL, MINT),
];
/// Moves per round: scramble, hold, solve, flash, rest.
const ROUND: u64 = 40;

#[derive(Default)]
struct Rubik {
    /// Moves made while waiting, two a second.
    waited: f64,
}

/// One moment of the puzzle.
struct Puzzle {
    /// Which of the two colors each sticker shows.
    stickers: [bool; 8],
    colors: (Rgb, Rgb),
    /// The move under way, if one is.
    moving: Option<usize>,
    /// A solved round's flash.
    flash: Option<Rgb>,
}

impl Rubik {
    fn puzzle(&self, stage: &Stage) -> Puzzle {
        let moves = (stage.tokens as f64 / 10.0 + self.waited).floor() as u64;
        let round = moves / ROUND;
        let step = (moves % ROUND) as usize;
        let seed = round as i64;
        let lucky = chance(11 * seed + 3, /*odds*/ 0.125);
        let length = if lucky { 3 } else { 12 };
        let mut scramble: Vec<usize> = Vec::with_capacity(length);
        for q in 0..length {
            let twist = (hash(97 * seed + q as i64 + 5) * 5.0) as usize;
            let repeats_undo = scramble.last().is_some_and(|&last| UNDO[last] == twist);
            scramble.push(if repeats_undo { (twist + 1) % 5 } else { twist });
        }
        let done = match step {
            0..12 => (step + 1).min(length),
            12..18 => length,
            18..30 => length.saturating_sub(step - 17),
            _ => 0,
        };
        let mut stickers = [false, false, false, false, true, true, true, true];
        for &twist in &scramble[..done] {
            stickers = TWISTS[twist].map(|from| stickers[from]);
        }
        let moving = if step < length {
            Some(scramble[step])
        } else if (18..18 + length).contains(&step) {
            Some(UNDO[scramble[length - 1 - (step - 18)]])
        } else {
            None
        };
        Puzzle {
            stickers,
            colors: SPLITS[(round % 5) as usize],
            moving,
            flash: (30..33)
                .contains(&step)
                .then_some(if lucky { GOLD } else { WHITE }),
        }
    }
}

impl Shimmer for Rubik {
    fn tick(&mut self, stage: &Stage) {
        if !stage.busy() {
            self.waited += 2.0 * TICK;
        }
    }

    fn status(&self, stage: &Stage, row: &mut Row) {
        let Puzzle {
            stickers,
            colors: (a, b),
            flash,
            ..
        } = self.puzzle(stage);
        let dim = |color: Rgb| {
            if stage.busy() {
                color
            } else {
                color.blend(stage.bg, /*alpha*/ 0.6)
            }
        };
        row.mark = [0, 4]
            .map(|first| match flash {
                Some(flash) => Ink::of('█').fg(flash.blend(stage.bg, /*alpha*/ 0.8)),
                None => {
                    let bits = (0..4)
                        .filter(|&q| !stickers[first + q])
                        .map(|q| QUADRANT_BITS[q])
                        .sum::<usize>();
                    Ink::of(QUADRANTS[bits]).fg(dim(a)).bg(dim(b))
                }
            })
            .to_vec();
        row.header = stage.letters(|i, _| {
            let sticker = flash.unwrap_or(if stickers[i % 8] { b } else { a });
            Ink::default().fg(dim(sticker.blend(stage.fg, /*alpha*/ 0.4)))
        });
    }

    fn tool(&self, stage: &Stage) -> Ink {
        match self.puzzle(stage).moving {
            Some(twist) => Ink::of(TWIST_SIGNS[twist]).fg(stage.grey(/*alpha*/ 0.85)),
            None => Ink::of('·').fg(stage.grey(/*alpha*/ 0.4)),
        }
    }
}
