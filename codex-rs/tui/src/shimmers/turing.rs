//! Turing: four blank-start machines rotate at work boundaries, never on repaint.

mod machine;

use rand::Rng as _;

use super::kit::AZURE;
use super::kit::CYAN;
use super::kit::GOLD;
use super::kit::ICE;
use super::kit::LIME;
use super::kit::PINK;
use super::kit::STEEL;
use super::kit::VIOLET;
use super::kit::WHITE;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Rgb;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;
use machine::Direction;
use machine::Machine;
use machine::State;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Turing",
    blurb: "four machines take turns on the tape",
    summon: || Box::<Turing>::default(),
};

const TAPE: usize = 37;
/// Paced tokens between transitions; waiting tokens are discarded, not caught up later.
const STEP: u64 = 12;
const TONES: [Rgb; 5] = [CYAN, GOLD, PINK, LIME, VIOLET];

struct Turing {
    machine: Machine,
    tape: [bool; TAPE],
    head: usize,
    /// No state means the selected machine reached its undefined (halt) instruction.
    state: Option<State>,
    direction: Direction,
    seen: u64,
}

impl Default for Turing {
    fn default() -> Self {
        Self::new(
            Machine::ALL[rand::rng().random_range(0..Machine::ALL.len())],
            0,
        )
    }
}

impl Turing {
    fn new(machine: Machine, seen: u64) -> Self {
        Self {
            machine,
            tape: [false; TAPE],
            head: TAPE / 2,
            state: Some(State::A),
            direction: Direction::Right,
            seen,
        }
    }

    fn step(&mut self) {
        let Some(state) = self.state else {
            return;
        };
        let Some(instruction) = self.machine.instruction(state, self.tape[self.head]) else {
            self.state = None;
            return;
        };
        self.tape[self.head] = instruction.write;
        self.head = match instruction.direction {
            Direction::Right => (self.head + 1) % TAPE,
            Direction::Left => (self.head + TAPE - 1) % TAPE,
        };
        self.direction = instruction.direction;
        self.state = Some(instruction.next);
    }

    fn tone(&self) -> Rgb {
        self.state.map_or(STEEL, |state| TONES[state as usize])
    }

    fn motion(&self) -> char {
        self.state.map_or('■', |_| self.direction.glyph())
    }
}

impl Shimmer for Turing {
    fn pause(&mut self, stage: &Stage) {
        *self = Self::new(self.machine.next(), stage.tokens / STEP);
    }

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
        let tone = self.tone();
        row.mark = vec![
            Ink::of(self.state.map_or('H', State::glyph))
                .fg(if busy { tone } else { rest })
                .bold(busy),
            Ink::of(if self.tape[self.head] { '1' } else { '0' }).fg(if busy { ICE } else { rest }),
            Ink::of(self.motion()).fg(if busy { tone } else { rest }),
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
        Ink::of(self.motion())
            .fg(if stage.busy() {
                self.tone()
            } else {
                stage.grey(/*alpha*/ 0.6)
            })
            .bold(stage.busy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shimmers::stage::Palette;
    use crate::shimmers::stage::Spell;

    #[test]
    fn work_boundaries_rotate_without_replaying_waiting_tokens() {
        let mut stage = Stage::new(Palette { fg: ICE, bg: STEEL });
        let mut machine = Turing::new(Machine::Wolfram, 0);
        for expected in Machine::ALL {
            assert_eq!(machine.machine, expected);
            assert_eq!(machine.tape, [false; TAPE]);
            assert_eq!(machine.state, Some(State::A));
            assert_eq!(machine.head, TAPE / 2);
            stage.advance(120, Spell::Busy);
            machine.tick(&stage);
            let running = (machine.tape, machine.head, machine.state, machine.direction);
            for _ in 0..3 {
                stage.advance(120, Spell::Wait);
                machine.tick(&stage);
                machine.status(&stage, &mut Row::default());
                assert_eq!(machine.machine, expected);
                assert_eq!(
                    (machine.tape, machine.head, machine.state, machine.direction),
                    running
                );
            }
            stage.advance(0, Spell::Busy);
            machine.tick(&stage);
            assert_eq!(
                (machine.tape, machine.head, machine.state, machine.direction),
                running
            );
            machine.pause(&stage);
            machine.tick(&stage);
        }
        assert_eq!(machine.machine, Machine::Wolfram);
        assert_eq!(machine.tape, [false; TAPE]);

        // The counter's undefined instruction remains a real halt, not a fifth invented rule.
        machine = Turing::new(Machine::Counter, 0);
        machine.state = Some(State::D);
        machine.tape[machine.head] = true;
        machine.step();
        assert_eq!(machine.state, None);
        machine.step();
        assert_eq!(machine.motion(), '■');
        machine.pause(&stage);
        assert_eq!(machine.state, Some(State::A));
    }
}
