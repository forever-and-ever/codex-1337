//! Prism: a narrow band of the spectrum across the header, fanned a little wider by fast output.

use super::kit::bump;
use super::kit::frac;
use super::kit::hsl;
use super::kit::lag;
use super::kit::smooth;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Prism",
    blurb: "a narrow band of the spectrum",
    summon: || Box::<Prism>::default(),
};

/// Tokens a second that light the whole header and fan the band widest.
const BRIGHTEST: f64 = 90.0;
/// Seconds a waiting ray takes to cross the header.
const RAY: f64 = 9.0;

#[derive(Default)]
struct Prism {
    /// The token rate, smoothed over about 0.6 seconds.
    flow: f64,
}

impl Prism {
    fn hue(stage: &Stage) -> f64 {
        0.03 * stage.t + stage.tokens as f64 / 3000.0
    }
}

impl Shimmer for Prism {
    fn tick(&mut self, stage: &Stage) {
        let flow = if stage.busy() {
            stage.flow(/*window*/ 0.5)
        } else {
            0.0
        };
        self.flow = lag(self.flow, flow, /*tau*/ 0.6);
    }

    fn status(&self, stage: &Stage, row: &mut Row) {
        let hue = Prism::hue(stage);
        let width = stage.header.len() as f64;
        let resting = stage.grey(/*alpha*/ 0.6);
        if !stage.busy() {
            let ray = frac(stage.t / RAY) * (width + 4.0) - 2.0;
            let faint = hsl(hue, /*saturation*/ 0.45, /*lightness*/ 0.62);
            row.mark = vec![Ink::of('◭').fg(stage.grey(/*alpha*/ 0.5)).bold(true)];
            row.header = stage.letters(|i, _| {
                Ink::default().fg(faint.blend(resting, bump(i as f64 + 0.5 - ray, /*width*/ 2.0)))
            });
            return;
        }
        let dispersion = (self.flow / BRIGHTEST).clamp(0.0, 1.0);
        let reach = 3.0 + dispersion * (width - 3.0);
        let band = 0.04 + 0.12 * dispersion;
        row.mark = vec![
            Ink::of('◭')
                .fg(hsl(hue, /*saturation*/ 0.35, /*lightness*/ 0.75))
                .bold(true),
        ];
        row.header = stage.letters(|i, _| {
            let x = (i as f64 + 0.5) / reach;
            let lit = 1.0 - smooth(/*from*/ 0.85, /*to*/ 1.15, x);
            let ray = hsl(
                hue + band * (x - 0.5),
                /*saturation*/ 0.5,
                /*lightness*/ 0.64,
            );
            Ink::default().fg(ray.blend(resting, lit)).bold(lit > 0.5)
        });
    }

    fn tool(&self, stage: &Stage) -> Ink {
        Ink::of('·')
            .fg(stage.grey(if stage.busy() { 0.85 } else { 0.4 }))
            .bold(true)
    }
}
