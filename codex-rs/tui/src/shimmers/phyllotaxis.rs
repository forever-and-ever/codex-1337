//! Phyllotaxis: tokens become florets placed at the golden angle.

use std::f64::consts::PI;

use super::kit::OCTANTS;
use super::kit::WHITE;
use super::kit::frac;
use super::kit::hsl;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Phyllotaxis",
    blurb: "a golden-angle seed head grows",
    summon: || Box::<Phyllotaxis>::default(),
};

const SEEDS: usize = 24;
const SEED: u64 = 11;
const GOLDEN: f64 = 2.399_963_229_728_653;
const BLOOM: f64 = 0.45;

#[derive(Clone, Copy, Default)]
struct Floret {
    x: f64,
    y: f64,
    hue: f64,
}

struct Phyllotaxis {
    florets: [Floret; SEEDS],
    count: usize,
    seen: u64,
    bloom: f64,
}

impl Default for Phyllotaxis {
    fn default() -> Self {
        Self {
            florets: [Floret::default(); SEEDS],
            count: 0,
            seen: 0,
            bloom: -BLOOM,
        }
    }
}

impl Shimmer for Phyllotaxis {
    #[expect(
        clippy::cast_precision_loss,
        reason = "Floret index and count are bounded by the 24-cell head; they only place visual seeds."
    )]
    fn tick(&mut self, stage: &Stage) {
        let target = stage.tokens / SEED;
        if !stage.busy() {
            self.seen = target;
            return;
        }
        while self.seen < target {
            self.seen += 1;
            let k = self.count;
            let radius = (((k + 1) as f64) / (SEEDS as f64)).sqrt() * 0.45;
            let angle = (k as f64) * GOLDEN;
            self.florets[k] = Floret {
                x: 0.5 + radius * angle.cos(),
                y: 0.5 + radius * angle.sin(),
                hue: frac((k as f64) * 0.381_966),
            };
            self.count += 1;
            if self.count == SEEDS {
                self.count = 0;
                self.bloom = stage.t;
            }
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "A six-by-four sprite deliberately quantizes visual seed coordinates and token/time scalars."
    )]
    fn status(&self, stage: &Stage, row: &mut Row) {
        let busy = stage.busy();
        let rest = stage.grey(/*alpha*/ 0.6);
        let bloom = (0.0..BLOOM).contains(&(stage.t - self.bloom));
        let mut plate = [None; 6 * 4];
        for floret in self.florets.iter().take(self.count) {
            let x = (floret.x * 6.0).clamp(0.0, 5.0) as usize;
            let y = (floret.y * 4.0).clamp(0.0, 3.0) as usize;
            plate[6 * y + x] = Some(*floret);
        }
        row.mark = (0..3)
            .map(|cell| {
                let mut bits = 0;
                let mut tone = rest;
                for y in 0..4 {
                    for x in 0..2 {
                        if let Some(floret) = plate[6 * y + 2 * cell + x] {
                            bits |= 1 << (2 * y + x);
                            tone = if bloom {
                                WHITE
                            } else {
                                hsl(
                                    floret.hue, /*saturation*/ 0.68, /*lightness*/ 0.62,
                                )
                            };
                        }
                    }
                }
                Ink::of(OCTANTS[bits])
                    .fg(if busy { tone } else { rest })
                    .bold(busy && bits != 0)
            })
            .collect();
        let token_hue = stage.tokens as f64 / 900.0;
        row.header = stage.letters(|i, _| {
            let hue = frac(i as f64 * 0.381_966 + token_hue);
            let lit = if busy {
                0.55 + 0.45 * (2.0 * PI * (hue - stage.t / 11.0)).sin()
            } else {
                0.3
            };
            Ink::default()
                .fg(if busy {
                    hsl(
                        hue,
                        /*saturation*/ 0.45,
                        /*lightness*/ 0.38 + 0.3 * lit,
                    )
                } else {
                    rest
                })
                .bold(busy && lit > 0.82)
        });
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "Token count only selects an approximate hue for the tool bullet."
    )]
    fn tool(&self, stage: &Stage) -> Ink {
        Ink::of('❋')
            .fg(if stage.busy() {
                hsl(
                    frac(stage.tokens as f64 / 900.0),
                    /*saturation*/ 0.65,
                    /*lightness*/ 0.66,
                )
            } else {
                stage.grey(/*alpha*/ 0.6)
            })
            .bold(stage.busy())
    }
}
