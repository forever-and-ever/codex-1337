//! Forge: output stokes a forge, the header glows from ember red to white heat, and waiting quenches it.

use std::f64::consts::PI;
use std::f64::consts::TAU;

use super::kit::GOLD;
use super::kit::ICE;
use super::kit::STEEL;
use super::kit::SUN;
use super::kit::TANGERINE;
use super::kit::WHITE;
use super::kit::chance;
use super::kit::hash;
use super::kit::lag;
use super::kit::pick;
use super::kit::ramp;
use super::kit::wobble;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Rgb;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Forge",
    blurb: "output stokes a forge",
    summon: || Box::<Forge>::default(),
};

const BLACKBODY: [Rgb; 6] = [
    Rgb(40.0, 8.0, 8.0),
    Rgb(140.0, 22.0, 12.0),
    Rgb(225.0, 60.0, 20.0),
    TANGERINE,
    SUN,
    Rgb(255.0, 250.0, 225.0),
];
const SHADES: [char; 4] = ['░', '▒', '▓', '█'];
/// Tokens per gold ingot, and the seconds its glow lasts.
const INGOT_TOKENS: u64 = 1000;
const INGOT: f64 = 1.5;
/// Seconds the wave of steam takes to rise off one letter.
const QUENCH: f64 = 1.4;

#[derive(Default)]
struct Forge {
    /// A half-second running average of the token rate.
    heat: f64,
    /// The workpiece's temperature, trailing the heat by about four seconds.
    work: f64,
    /// When the last ingot was struck.
    struck: Option<f64>,
}

impl Shimmer for Forge {
    fn tick(&mut self, stage: &Stage) {
        let fire = if stage.busy() {
            stage.flow(/*window*/ 0.25) / 100.0
        } else {
            0.0
        };
        self.heat = lag(self.heat, fire, /*tau*/ 0.5);
        self.work = lag(self.work, self.heat, /*tau*/ 4.0);
        let before = stage.tokens - u64::from(stage.fresh());
        if stage.tokens / INGOT_TOKENS > before / INGOT_TOKENS {
            self.struck = Some(stage.t);
        }
    }

    fn status(&self, stage: &Stage, row: &mut Row) {
        let t = stage.t;
        let flicker = (t / 0.096).floor() as i64;
        row.tail = (0..3)
            .map(|j| {
                if stage.busy() && chance(3 * flicker + j, 3.0 * (self.heat - 0.85)) {
                    Ink::of(pick(5 * flicker + j, &['·', '*', '\'']))
                        .fg(pick(7 * flicker + j, &[SUN, WHITE, TANGERINE]))
                } else {
                    Ink::of(' ')
                }
            })
            .collect();
        if !stage.busy() {
            // A wave of steam rises off the letters the moment waiting begins.
            let puff = (t / 0.16).floor() as usize;
            let vapor = ICE.blend(stage.bg, /*alpha*/ 0.5);
            let banked = STEEL.blend(stage.grey(/*alpha*/ 0.6), /*alpha*/ 0.35);
            let steaming = |i: usize| {
                let rise = (stage.spell_t - 0.03 * i as f64) / QUENCH;
                t > stage.spell_t
                    && rise > 0.0
                    && rise < 1.0
                    && hash(97 * i as i64 + stage.headers) < (PI * rise).sin()
            };
            row.mark = vec![Ink::of('░').fg(ramp(&BLACKBODY, 0.2 + 0.1 * (TAU * t / 3.0).sin()))];
            row.header = stage.letters(|i, _| {
                if steaming(i) {
                    Ink::of(['░', '▒'][(i + puff) % 2]).fg(vapor)
                } else {
                    Ink::default().fg(banked)
                }
            });
            return;
        }
        if let Some(age) = self.struck.map(|at| t - at).filter(|age| *age < INGOT) {
            let tone = GOLD
                .blend(stage.fg, /*alpha*/ 0.5)
                .blend(ramp(&BLACKBODY, /*x*/ 0.6), 1.0 - age / INGOT);
            row.mark = vec![Ink::of('■').fg(GOLD).bold(true)];
            row.header = stage.letters(|_, _| Ink::default().fg(tone));
            return;
        }
        let reach = stage.header.len() as f64 + 6.0;
        let slot = (t / 0.4).floor();
        let flare = |i: i64| {
            if chance(131 * i + slot as i64, /*odds*/ 0.04) {
                0.3 * (PI * (t / 0.4 - slot)).sin()
            } else {
                0.0
            }
        };
        let shade = SHADES[((4.0 * self.heat).floor().max(0.0) as usize).min(SHADES.len() - 1)];
        row.mark = vec![
            Ink::of(shade)
                .fg(ramp(&BLACKBODY, 0.2 + 0.8 * self.heat))
                .bold(true),
        ];
        row.header = stage.letters(|i, _| {
            let i = i as i64;
            let glow = self.heat
                * (1.0 - i as f64 / reach)
                * (0.92 + 0.16 * wobble(977 * i, t, /*period*/ 0.3))
                + flare(i);
            Ink::default()
                .fg(ramp(&BLACKBODY, 0.25 + 0.75 * glow))
                .bold(glow > 0.6)
        });
    }

    fn tool(&self, _stage: &Stage) -> Ink {
        Ink::of('▪').fg(ramp(&BLACKBODY, 0.15 + self.work))
    }
}
