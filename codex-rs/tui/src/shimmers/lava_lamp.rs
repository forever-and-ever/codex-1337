//! Lava Lamp: wax blobs drift behind the status row and merge, heaving with output and settling to wait.

use std::f64::consts::TAU;

use super::kit::Lottery;
use super::kit::PINK;
use super::kit::SUN;
use super::kit::TANGERINE;
use super::kit::VIOLET;
use super::kit::hash;
use super::kit::smooth;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Rgb;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;
use super::stage::TICK;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Lava Lamp",
    blurb: "wax drifts behind the status row",
    summon: || Box::<LavaLamp>::default(),
};

const WAX: [Rgb; 3] = [PINK, TANGERINE, VIOLET];
/// Periods, in lamp seconds, of each blob's drift along the row, heave across it and swell.
const DRIFT: [f64; 3] = [13.1, 17.3, 23.9];
const HEAVE: [f64; 3] = [5.3, 7.1, 6.2];
const SWELL: [f64; 3] = [4.1, 5.7, 3.3];
/// Columns of the status row the lamp lights.
const GLASS: usize = 34;
const BUBBLE: Lottery = Lottery {
    every: 13.0,
    odds: 0.5,
    span: 1.5,
    seed: 2,
};

#[derive(Default)]
struct LavaLamp {
    /// The lamp's own clock, running a third as fast while the agent waits.
    t: f64,
}

struct Blob {
    wax: Rgb,
    x: f64,
    y: f64,
    radius: f64,
}

impl Shimmer for LavaLamp {
    fn tick(&mut self, stage: &Stage) {
        self.t += if stage.busy() { TICK } else { 0.35 * TICK };
    }

    fn status(&self, stage: &Stage, row: &mut Row) {
        let t = self.t;
        let heat = (stage.tokens_in(/*window*/ 3.0) / 240.0).min(1.0);
        let wave = |period: f64, phase: f64| (TAU * t / period + phase).sin();
        let mut blobs: Vec<Blob> = WAX
            .iter()
            .enumerate()
            .map(|(j, &wax)| {
                let phase = j as f64;
                Blob {
                    wax,
                    x: 2.0 + 30.0 * (0.5 + 0.5 * wave(DRIFT[j], 2.0 * phase)),
                    y: 0.25 + heat * (0.8 * wave(HEAVE[j], phase) - 0.3),
                    radius: 2.6 + 0.8 * wave(SWELL[j], /*phase*/ 0.0),
                }
            })
            .collect();
        if stage.busy()
            && let Some(rise) = BUBBLE.at(stage.t)
        {
            blobs.push(Blob {
                wax: SUN,
                x: 4.0 + 26.0 * hash((stage.t / BUBBLE.every).floor() as i64),
                y: 1.0 - 2.0 * rise,
                radius: 1.1,
            });
        }
        let liquid = if stage.busy() {
            Rgb(90.0, 30.0, 120.0)
        } else {
            Rgb(25.0, 40.0, 120.0)
        }
        .blend(stage.bg, /*alpha*/ 0.15);
        row.backdrop = (0..GLASS)
            .map(|column| {
                let (mut field, mut sum) = (0.0, Rgb(0.0, 0.0, 0.0));
                for blob in &blobs {
                    let reach = blob.radius * blob.radius
                        / ((column as f64 + 0.5 - blob.x).powi(2) + (2.2 * blob.y).powi(2) + 0.3);
                    field += reach;
                    sum = Rgb(
                        sum.0 + reach * blob.wax.0,
                        sum.1 + reach * blob.wax.1,
                        sum.2 + reach * blob.wax.2,
                    );
                }
                let wax = Rgb(sum.0 / field, sum.1 / field, sum.2 / field)
                    .blend(stage.bg, /*alpha*/ 0.275);
                if field > 1.0 {
                    wax
                } else {
                    wax.blend(liquid, 0.5 * smooth(/*from*/ 0.5, /*to*/ 1.0, field))
                }
            })
            .collect();
        row.mark = vec![Ink::of('●').fg(stage.grey(/*alpha*/ 0.85))];
        if !stage.busy() {
            let resting = stage.grey(/*alpha*/ 0.6);
            row.header = stage.letters(|_, _| Ink::default().fg(resting));
        }
    }

    fn tool(&self, stage: &Stage) -> Ink {
        let drop = WAX[(self.t / 7.0).floor() as usize % WAX.len()];
        Ink::of('•').fg(drop.blend(stage.grey(/*alpha*/ 0.6), /*alpha*/ 0.5))
    }
}
