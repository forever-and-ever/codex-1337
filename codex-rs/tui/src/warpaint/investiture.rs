//! Investiture: while a pack is worn, its ink and canvas are the terminal's default colors,
//! published over OSC 10 and 11 so that everything the terminal paints by default (cleared rows,
//! the margin, tool output's resets) agrees with the pack. Only a terminal that reported its own
//! default colors at startup is invested, because only those colors can be restored; the
//! terminal's colors come back whenever the TUI lets go of the screen.

use std::io;
use std::io::Write;

use super::bind::Canvas;
use super::pack::Rgb;
use crate::terminal_probe::DefaultColors;

pub(super) enum Investiture {
    /// The terminal's own colors are unknown, so none are replaced: startup has not asked for
    /// them, or the terminal did not answer.
    Unknown,
    /// The terminal reported its colors; `worn` is the pack published over them.
    Known {
        prior: DefaultColors,
        worn: Option<usize>,
    },
}

impl Investiture {
    pub(super) fn note(colors: Option<DefaultColors>) -> Self {
        colors.map_or(Self::Unknown, |prior| Self::Known { prior, worn: None })
    }

    /// The terminal's own colors, if it reported them.
    #[cfg(all(unix, not(test)))]
    pub(super) fn prior(&self) -> Option<DefaultColors> {
        match self {
            Self::Unknown => None,
            Self::Known { prior, .. } => Some(*prior),
        }
    }

    /// Publishes pack `worn`'s ink and canvas unless they already are.
    pub(super) fn invest(
        &mut self,
        terminal: &mut impl Write,
        worn: usize,
        ink: Rgb,
        canvas: Rgb,
    ) -> io::Result<Canvas> {
        let Self::Known {
            worn: published, ..
        } = self
        else {
            return Ok(Canvas::Painted);
        };
        if *published != Some(worn) {
            publish(terminal, ink.tuple(), canvas.tuple())?;
            *published = Some(worn);
        }
        Ok(Canvas::Invested)
    }

    /// Restores the terminal's own colors, if a pack's were published over them.
    pub(super) fn revoke(&mut self, terminal: &mut impl Write) -> io::Result<()> {
        if let Self::Known {
            prior,
            worn: worn @ Some(_),
        } = self
        {
            publish(terminal, prior.fg, prior.bg)?;
            terminal.flush()?;
            *worn = None;
        }
        Ok(())
    }
}

fn publish(terminal: &mut impl Write, fg: (u8, u8, u8), bg: (u8, u8, u8)) -> io::Result<()> {
    for (slot, (r, g, b)) in [(10, fg), (11, bg)] {
        write!(terminal, "\x1b]{slot};rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// The terminal's colors are borrowed: each pack is published once, and letting go of the
    /// screen returns exactly what the terminal reported, once. A terminal that reported nothing
    /// is never touched, since nothing could be restored.
    #[test]
    fn revoke_returns_the_colors_the_terminal_reported() -> io::Result<()> {
        let prior = DefaultColors {
            fg: (0xe0, 0xe1, 0xe2),
            bg: (0x10, 0x11, 0x12),
        };
        let (ink, canvas) = (Rgb(0xa0, 0xa1, 0xa2), Rgb(0x20, 0x21, 0x22));
        let mut terminal = Vec::new();
        let mut investiture = Investiture::note(Some(prior));
        for worn in [3, 3, 5] {
            assert!(matches!(
                investiture.invest(&mut terminal, worn, ink, canvas)?,
                Canvas::Invested
            ));
        }
        investiture.revoke(&mut terminal)?;
        investiture.revoke(&mut terminal)?;
        investiture.invest(&mut terminal, 5, ink, canvas)?;
        let pack = "\x1b]10;rgb:a0/a1/a2\x1b\\\x1b]11;rgb:20/21/22\x1b\\";
        let own = "\x1b]10;rgb:e0/e1/e2\x1b\\\x1b]11;rgb:10/11/12\x1b\\";
        assert_eq!(
            String::from_utf8_lossy(&terminal),
            [pack, pack, own, pack].concat()
        );

        let mut untouched = Vec::new();
        let mut unknown = Investiture::note(None);
        assert!(matches!(
            unknown.invest(&mut untouched, 3, ink, canvas)?,
            Canvas::Painted
        ));
        unknown.revoke(&mut untouched)?;
        assert_eq!(untouched, [] as [u8; 0]);
        Ok(())
    }
}
