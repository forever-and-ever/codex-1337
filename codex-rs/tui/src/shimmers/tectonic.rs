//! Tectonic: output loads a fault until the plates slip and a wave crosses the header.

use super::kit::AZURE;
use super::kit::CORAL;
use super::kit::GOLD;
use super::kit::STEEL;
use super::kit::WHITE;
use super::kit::bump;
use super::kit::chance;
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
    fault: usize,
}

impl Default for Tectonic {
    fn default() -> Self {
        Self {
            stress: 0.0,
            seen: 0,
            quake: -SLIP,
            fault: 0,
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
            let gaps = stage.header.len().saturating_sub(1);
            if gaps > 0 {
                self.fault %= gaps;
                self.fault = if chance(i64::try_from(self.seen).unwrap_or(i64::MAX), 0.5) {
                    (self.fault + 1) % gaps
                } else {
                    self.fault.checked_sub(1).unwrap_or(gaps - 1)
                };
            }
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
        let fault = if width > 1 {
            1 + self.fault % (width - 1)
        } else {
            width
        };
        let age = stage.t - self.quake;
        let wave = busy && (0.0..SLIP).contains(&age);
        let light = |column: usize| {
            if wave {
                bump(
                    f64::from(u32::try_from(column.abs_diff(fault)).unwrap_or(u32::MAX))
                        - age * WAVE,
                    /*width*/ 3.1,
                )
            } else {
                0.0
            }
        };
        row.header = stage.letters(|i, _| {
            let side = if i < fault { CORAL } else { AZURE };
            let lit = light(i + usize::from(i >= fault));
            let mut ink = Ink::default().fg(if busy {
                side.blend(STEEL, /*alpha*/ 0.38 - 0.18 * self.stress + 0.1 * lit)
            } else {
                rest
            });
            if busy {
                let alpha = 0.035 + 0.04 * self.stress + 0.3 * lit;
                ink = ink.bg(if lit > 0.25 { WHITE } else { side }.blend(stage.bg, alpha));
                ink = ink.bold(lit > 0.48);
            }
            ink
        });
        if busy && width > 1 {
            let lit = light(fault);
            row.header_insert = Some((
                fault,
                Ink::of('│')
                    .fg(AZURE.blend(STEEL, 0.38 - 0.18 * self.stress + 0.1 * lit))
                    .bg(if lit > 0.25 { WHITE } else { AZURE }.blend(stage.bg, 0.11 + 0.3 * lit))
                    .bold(lit > 0.48),
            ));
        }
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
