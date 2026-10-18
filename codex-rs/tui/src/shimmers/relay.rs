//! Relay: an eight-bit shift register walks a local bus across the header.

use super::kit::AZURE;
use super::kit::CYAN;
use super::kit::ICE;
use super::kit::STEEL;
use super::kit::SUN;
use super::kit::WHITE;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Relay",
    blurb: "a relay register walks the bus",
    summon: || Box::<Relay>::default(),
};

const SHIFT: u64 = 12;
const BUS_HALF_WIDTH: usize = 4;
const ARC: f64 = 0.36;

struct Relay {
    bits: u8,
    seen: u64,
    pulse: f64,
}

impl Default for Relay {
    fn default() -> Self {
        Self {
            bits: 1,
            seen: 0,
            pulse: -ARC,
        }
    }
}

impl Shimmer for Relay {
    fn tick(&mut self, stage: &Stage) {
        let target = stage.tokens / SHIFT;
        if !stage.busy() {
            self.seen = target;
            return;
        }
        while self.seen < target {
            self.seen += 1;
            let feedback = ((self.bits >> 7) ^ (self.bits >> 2)) & 1;
            let next = self.bits.wrapping_shl(1) | feedback;
            if next != self.bits {
                self.pulse = stage.t;
            }
            self.bits = next;
        }
    }

    fn status(&self, stage: &Stage, row: &mut Row) {
        let busy = stage.busy();
        let rest = stage.grey(/*alpha*/ 0.6);
        let width = stage.header.len();
        let head = if width == 0 {
            0
        } else {
            usize::try_from(stage.tokens / SHIFT).unwrap_or_default() % width
        };
        let arc = busy && stage.t - self.pulse < ARC;
        row.header = stage.letters(|i, _| {
            let bit = (self.bits >> (i % 8)) & 1 == 1;
            let here = i == head;
            let near = i.abs_diff(head) <= BUS_HALF_WIDTH;
            let mut ink = Ink::default()
                .fg(if !busy {
                    rest
                } else if here {
                    WHITE
                } else if near && bit {
                    ICE
                } else {
                    rest
                })
                .bold(busy && (here || bit));
            if busy {
                if here {
                    ink = ink.bg(if arc { SUN } else { AZURE }.blend(stage.bg, /*alpha*/ 0.42));
                } else if near && bit {
                    ink = ink.bg(AZURE.blend(stage.bg, /*alpha*/ 0.13));
                }
            }
            ink
        });
        row.mark = (0..3)
            .map(|bit| {
                let on = (self.bits >> bit) & 1 == 1;
                Ink::of(if on { '●' } else { '○' })
                    .fg(if busy {
                        if on { CYAN } else { STEEL }
                    } else {
                        rest
                    })
                    .bold(busy && on)
            })
            .collect();
    }

    fn tool(&self, stage: &Stage) -> Ink {
        let busy = stage.busy();
        let arc = busy && stage.t - self.pulse < ARC;
        Ink::of(if arc { 'ϟ' } else { '⌁' })
            .fg(if busy {
                if arc { SUN } else { CYAN }
            } else {
                stage.grey(/*alpha*/ 0.6)
            })
            .bold(busy)
    }
}
