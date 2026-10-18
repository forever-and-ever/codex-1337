//! Turing: a three-state machine edits a cyclic tape under the status header.

use super::kit::AZURE;
use super::kit::CYAN;
use super::kit::GOLD;
use super::kit::ICE;
use super::kit::PINK;
use super::kit::WHITE;
use super::kit::hash;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Rgb;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Turing",
    blurb: "a three-state machine edits the tape",
    summon: || Box::<Turing>::default(),
};

/// Cells in the cyclic tape.
const TAPE: usize = 37;
/// Paced tokens between transitions.
const STEP: u64 = 12;
const STATES: [char; 3] = ['A', 'B', 'C'];
const TONES: [Rgb; 3] = [CYAN, GOLD, PINK];
/// `(write, move, next)` for each state and tape symbol.
const RULES: [[(bool, i8, usize); 2]; 3] = [
    [(true, 1, 1), (false, -1, 2)],
    [(true, 1, 2), (true, -1, 0)],
    [(false, -1, 0), (true, 1, 1)],
];

struct Turing {
    tape: [bool; TAPE],
    head: usize,
    state: usize,
    direction: i8,
    seen: u64,
}

impl Default for Turing {
    fn default() -> Self {
        let mut seed = 37_i64;
        let tape = std::array::from_fn(|_| {
            let bit = hash(seed) < 0.48;
            seed += 1;
            bit
        });
        Self {
            tape,
            head: 18,
            state: 0,
            direction: 1,
            seen: 0,
        }
    }
}

impl Turing {
    fn step(&mut self) {
        let (write, direction, state) = RULES[self.state][usize::from(self.tape[self.head])];
        self.tape[self.head] = write;
        self.head = if direction > 0 {
            (self.head + 1) % TAPE
        } else {
            (self.head + TAPE - 1) % TAPE
        };
        self.direction = direction;
        self.state = state;
    }
}

impl Shimmer for Turing {
    fn tick(&mut self, stage: &Stage) {
        let target = stage.tokens / STEP;
        if !stage.busy() {
            self.seen = target;
            return;
        }
        while self.seen < target {
            self.seen += 1;
            self.step();
        }
    }

    fn status(&self, stage: &Stage, row: &mut Row) {
        let busy = stage.busy();
        let rest = stage.grey(/*alpha*/ 0.6);
        let tone = TONES[self.state];
        row.mark = vec![
            Ink::of(STATES[self.state])
                .fg(if busy { tone } else { rest })
                .bold(busy),
            Ink::of(if self.tape[self.head] { '1' } else { '0' }).fg(if busy { ICE } else { rest }),
            Ink::of(if self.direction > 0 { '▸' } else { '◂' }).fg(if busy { tone } else { rest }),
        ];
        let width = stage.header.len();
        if width == 0 {
            return;
        }
        let focus = self.head % width;
        row.header = stage.letters(|i, _| {
            let bit = self.tape[(self.head + TAPE + i - focus) % TAPE];
            let here = i == focus && busy;
            let mut ink = Ink::default().fg(if !busy {
                rest
            } else if here {
                WHITE
            } else if bit {
                ICE
            } else {
                rest
            });
            if busy {
                if here {
                    ink = ink.bg(tone.blend(stage.bg, /*alpha*/ 0.5));
                } else if bit {
                    ink = ink.bg(AZURE.blend(stage.bg, /*alpha*/ 0.12));
                }
            }
            ink.bold(here)
        });
    }

    fn tool(&self, stage: &Stage) -> Ink {
        Ink::of(if self.direction > 0 { '▸' } else { '◂' })
            .fg(if stage.busy() {
                TONES[self.state]
            } else {
                stage.grey(/*alpha*/ 0.6)
            })
            .bold(stage.busy())
    }
}
