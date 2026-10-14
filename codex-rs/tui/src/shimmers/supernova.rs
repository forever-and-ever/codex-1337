//! Supernova: a star grows with the output, lighting the header farther as it heats, until it blows.

use super::kit::AZURE;
use super::kit::CORAL;
use super::kit::CYAN;
use super::kit::ICE;
use super::kit::SUN;
use super::kit::TANGERINE;
use super::kit::VIOLET;
use super::kit::WHITE;
use super::kit::chance;
use super::kit::frac;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Rgb;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Supernova",
    blurb: "a star grows with output, then blows",
    summon: || Box::<Supernova>::default(),
};

const STELLAR: [Rgb; 7] = [CORAL, TANGERINE, SUN, ICE, CYAN, AZURE, VIOLET];
const STARS: [char; 7] = ['·', '✦', '✶', '✷', '✸', '✹', '✺'];
/// Tokens per stage of a star's life, when it goes nova, when the remnant settles, and when a new
/// star is born.
const STAGE: u64 = 40;
const NOVA: u64 = 280;
const REMNANT: u64 = 320;
const LIFE: u64 = 360;
/// Letters per second the shock front crosses, and seconds a letter takes to fade behind it.
const FRONT: f64 = 30.0;
const FADE: f64 = 0.7;

#[derive(Default)]
struct Supernova {
    /// When the current star went nova.
    nova: f64,
}

impl Shimmer for Supernova {
    fn tick(&mut self, stage: &Stage) {
        let crossings = |tokens: u64| (tokens + LIFE - NOVA) / LIFE;
        if crossings(stage.tokens) > crossings(stage.tokens - u64::from(stage.fresh())) {
            self.nova = stage.t;
        }
    }

    fn status(&self, stage: &Stage, row: &mut Row) {
        let t = stage.t;
        let life = stage.tokens % LIFE;
        let star = (stage.tokens / LIFE) as i64;
        let hyper = chance(31 * star + 7, /*odds*/ 0.25);
        if life < NOVA {
            let phase = (life / STAGE) as usize;
            // A waiting star twinkles: now and then it shows the stage before.
            let shown =
                if stage.busy() || !chance(7 * (t / 0.3).floor() as i64 + 1, /*odds*/ 0.3) {
                    phase
                } else {
                    phase.saturating_sub(1)
                };
            let tone = STELLAR[phase].blend(WHITE, /*alpha*/ 0.75);
            let reach = 3.0 + 4.0 * phase as f64;
            let heat = STELLAR[phase];
            row.mark = vec![
                Ink::of(STARS[shown])
                    .fg(if stage.busy() {
                        heat
                    } else {
                        heat.blend(stage.bg, /*alpha*/ 0.5)
                    })
                    .bold(true),
            ];
            let resting = stage.grey(/*alpha*/ 0.6);
            row.header = stage.letters(|i, _| {
                Ink::default().fg(if stage.busy() {
                    tone.blend(stage.grey(/*alpha*/ 0.42), (-(i as f64) / reach).exp())
                } else {
                    resting
                })
            });
            return;
        }
        let age = t - self.nova;
        let front = if hyper { VIOLET } else { ICE }.blend(WHITE, /*alpha*/ 0.5);
        row.mark = vec![if age < 0.25 {
            Ink::of('✺')
                .fg(WHITE)
                .bg(WHITE.blend(stage.bg, /*alpha*/ 0.3))
                .bold(true)
        } else if life < REMNANT {
            Ink::of('✺').fg(if hyper { VIOLET } else { SUN }).bold(true)
        } else if hyper {
            Ink::of('●')
                .fg(stage.bg)
                .bg(VIOLET.blend(stage.bg, /*alpha*/ 0.7))
        } else {
            Ink::of(if (t / 0.128).floor() as i64 % 2 == 1 {
                '•'
            } else {
                '·'
            })
            .fg(CYAN)
            .bold(true)
        }];
        let unlit = VIOLET.blend(WHITE, /*alpha*/ 0.75);
        row.header = stage.letters(|i, _| {
            let i = i as f64;
            let behind = age - i / FRONT;
            if behind < 0.0 {
                return Ink::default()
                    .fg(unlit.blend(stage.grey(/*alpha*/ 0.42), (-i / 27.0).exp()));
            }
            let ink = if behind < FADE {
                Ink::default()
                    .fg(front.blend(stage.grey(/*alpha*/ 0.5), 1.0 - behind / FADE))
                    .bold(behind < 0.3)
            } else {
                Ink::default().fg(stage.grey(/*alpha*/ 0.5))
            };
            if hyper && behind < 1.6 {
                ink.bg(VIOLET.blend(stage.bg, 0.25 * (1.0 - behind / 1.6)))
            } else {
                ink
            }
        });
    }

    fn tool(&self, stage: &Stage) -> Ink {
        Ink::of('✦').fg(if frac(stage.t / 1.7) < 0.5 {
            ICE.blend(stage.bg, /*alpha*/ 0.5)
        } else {
            ICE
        })
    }
}
