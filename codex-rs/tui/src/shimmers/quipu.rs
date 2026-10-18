//! Quipu: four pendant cords tally the token inflow in base four.

use super::kit::CORAL;
use super::kit::CYAN;
use super::kit::SUN;
use super::kit::VIOLET;
use super::kit::WHITE;
use super::kit::bump;
use super::kit::frac;
use super::stage::Ink;
use super::stage::Recipe;
use super::stage::Rgb;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Stage;

pub(super) const RECIPE: Recipe = Recipe {
    name: "Quipu",
    blurb: "four knotted cords tally the output",
    summon: || Box::new(Quipu),
};

const KNOTS: [char; 4] = ['┆', '┼', '╫', '※'];
const CORDS: [Rgb; 4] = [SUN, CORAL, CYAN, VIOLET];

struct Quipu;

impl Shimmer for Quipu {
    #[expect(
        clippy::cast_precision_loss,
        reason = "The reading knot and header positions are approximate visual coordinates; exact token accounting remains upstream."
    )]
    fn status(&self, stage: &Stage, row: &mut Row) {
        let busy = stage.busy();
        let rest = stage.grey(/*alpha*/ 0.6);
        let digits = [
            (stage.tokens / 64) % 4,
            (stage.tokens / 16) % 4,
            (stage.tokens / 4) % 4,
            stage.tokens % 4,
        ];
        row.mark = digits
            .iter()
            .enumerate()
            .map(|(i, &digit)| {
                let knot = usize::try_from(digit).unwrap_or_default();
                Ink::of(KNOTS[knot])
                    .fg(if busy { CORDS[i] } else { rest })
                    .bold(busy && digit > 0)
            })
            .collect();
        let width = f64::from(u32::try_from(stage.header.len()).unwrap_or(u32::MAX));
        let read = frac(stage.tokens as f64 / 128.0) * (width + 3.0) - 1.5;
        row.header = stage.letters(|i, _| {
            let cord = CORDS[i % 4];
            let near = if busy {
                bump(
                    f64::from(u32::try_from(i).unwrap_or(u32::MAX)) + 0.5 - read,
                    /*width*/ 1.5,
                )
            } else {
                0.0
            };
            let mut ink = Ink::default()
                .fg(cord.blend(if near > 0.5 { WHITE } else { rest }, 0.3 + 0.55 * near))
                .bold(busy && near > 0.55);
            if busy {
                ink = ink.bg(cord.blend(stage.bg, /*alpha*/ 0.04 + 0.16 * near));
            }
            ink
        });
    }

    fn tool(&self, stage: &Stage) -> Ink {
        Ink::of('┆')
            .fg(if stage.busy() {
                CORDS[usize::try_from(stage.tokens % 4).unwrap_or_default()]
            } else {
                stage.grey(/*alpha*/ 0.6)
            })
            .bold(stage.busy())
    }
}
