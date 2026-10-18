//! Tectonic: output loads a fault until the plates slip and a wave crosses the header.

use super::kit::AZURE;
use super::kit::CORAL;
use super::kit::GOLD;
use super::kit::STEEL;
use super::kit::WHITE;
use super::kit::bump;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;
use super::stage::TICK;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Tectonic",
    blurb: "two plates grind and slip",
    summon: || Box::<Tectonic>::default(),
};

const LOAD: u64 = 8;
const STRESS: f64 = 0.034;
const RELIEF: f64 = 0.012;
const SLIP: f64 = 0.9;
const WAVE: f64 = 55.0;
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

struct Tectonic {
    stress: f64,
    seen: u64,
    quake: f64,
    slip: usize,
}

impl Default for Tectonic {
    fn default() -> Self {
        Self {
            stress: 0.0,
            seen: 0,
            quake: -SLIP,
            slip: 0,
        }
    }
}

impl Shimmer for Tectonic {
    fn tick(&mut self, stage: &Stage) {
        let target = stage.tokens / LOAD;
        if stage.busy() {
            if target > self.seen {
                let added = u32::try_from(target - self.seen).unwrap_or(u32::MAX);
                self.stress += STRESS * f64::from(added);
                self.seen = target;
            }
        } else {
            self.seen = target;
        }
        self.stress = (self.stress - RELIEF * TICK).max(0.0);
        if self.stress >= 1.0 {
            self.stress = 0.0;
            self.quake = stage.t;
            self.slip ^= 1;
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "The strain gauges deliberately quantize a clamped visual level into a block glyph."
    )]
    fn status(&self, stage: &Stage, row: &mut Row) {
        let busy = stage.busy();
        let rest = stage.grey(/*alpha*/ 0.6);
        let width = stage.header.len();
        let fault = if width == 0 {
            0
        } else {
            (width / 2 + self.slip).min(width - 1)
        };
        let age = stage.t - self.quake;
        let wave = busy && (0.0..SLIP).contains(&age);
        row.header = stage.letters(|i, _| {
            let side = if i < fault { CORAL } else { AZURE };
            let lit = if wave {
                bump(
                    (f64::from(u32::try_from(i).unwrap_or(u32::MAX))
                        - f64::from(u32::try_from(fault).unwrap_or(u32::MAX)))
                    .abs()
                        - age * WAVE,
                    /*width*/ 3.1,
                )
            } else {
                0.0
            };
            let mut ink = Ink::default().fg(if busy {
                side.blend(STEEL, /*alpha*/ 0.38 - 0.18 * self.stress + 0.1 * lit)
            } else {
                rest
            });
            if busy {
                let alpha = if i == fault {
                    0.11 + 0.3 * lit
                } else {
                    0.035 + 0.04 * self.stress + 0.3 * lit
                };
                ink = ink.bg(if lit > 0.25 { WHITE } else { side }.blend(stage.bg, alpha));
                ink = ink.bold(lit > 0.48);
                if i == fault {
                    ink.glyph = Some('│');
                }
            }
            ink
        });
        row.mark = (0..3)
            .map(|gauge| {
                let level = (self.stress * 3.0 - f64::from(gauge)).clamp(0.0, 1.0);
                Ink::of(BLOCKS[(level * 7.0).round() as usize])
                    .fg(if busy { GOLD.blend(CORAL, level) } else { rest })
                    .bold(busy && level > 0.25)
            })
            .collect();
    }

    fn tool(&self, stage: &Stage) -> Ink {
        Ink::of('⌁')
            .fg(if stage.busy() {
                CORAL
            } else {
                stage.grey(/*alpha*/ 0.6)
            })
            .bold(stage.busy())
    }
}
