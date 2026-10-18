//! The interface between the engine and content: what a shimmer reads each tick and what it paints.

/// Seconds per tick; codex-tui schedules animation frames 32 ms apart.
pub(super) const TICK: f64 = 0.032;
/// Ticks of paced token history a stage keeps, a little over four seconds.
const HISTORY: usize = 128;
/// Seconds over which a lump of tokens entering the context drains into the stage.
pub(super) const DRAIN: f64 = 4.0;
/// Tokens a second above which the stage's pace grows only logarithmically.
const KNEE: f64 = 100.0;

/// The stage's pace, in tokens a second, for `rate` tokens a second entering the context: the rate
/// itself up to [`KNEE`], and logarithmic beyond, so a large result surges without swamping the row.
pub(super) fn pace(rate: f64) -> f64 {
    if rate <= KNEE {
        rate
    } else {
        KNEE * (1.0 + (rate / KNEE).ln())
    }
}

/// A stock shimmer: its picker entry and how to start one.
pub(super) struct Recipe {
    pub(super) name: &'static str,
    pub(super) blurb: &'static str,
    pub(super) summon: fn() -> Box<dyn Shimmer>,
}

/// A live-row animation, written like a shader against the [`Stage`] the engine hands it.
pub(super) trait Shimmer: Send {
    /// Advances the shimmer's own state by one tick.
    fn tick(&mut self, _stage: &Stage) {}
    /// Paints the status row.
    fn status(&self, stage: &Stage, row: &mut Row);
    /// Paints a running tool's bullet, which is one cell wide.
    fn tool(&self, stage: &Stage) -> Ink;
}

/// Whether the agent is producing output or waiting on a tool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Spell {
    #[default]
    Busy,
    Wait,
}

/// The uniforms of one tick.
pub(super) struct Stage {
    /// Seconds on the shimmer's clock, which runs only while live rows are drawn.
    pub(super) t: f64,
    pub(super) spell: Spell,
    /// Seconds into the current spell; the first spell starts with the clock.
    pub(super) spell_t: f64,
    /// Paced tokens since the shimmer was engaged: the displayed thread's context inflow, at the
    /// [`pace`] the engine feeds it.
    pub(super) tokens: u64,
    /// Seconds since a paced token arrived.
    pub(super) quiet: f64,
    /// The status header, one entry per grapheme.
    pub(super) header: Vec<String>,
    /// Seconds since this header appeared.
    pub(super) header_t: f64,
    /// Headers seen so far, counting this one.
    pub(super) headers: i64,
    /// Whole seconds on the status row's timer.
    pub(super) timer: u64,
    /// The terminal's default foreground.
    pub(super) fg: Rgb,
    /// The terminal's default background.
    pub(super) bg: Rgb,
    /// Paced tokens that arrived in each recent tick; `newest` indexes the last.
    history: [u32; HISTORY],
    newest: usize,
}

impl Stage {
    pub(super) fn new(palette: Palette) -> Self {
        Self {
            t: 0.0,
            spell: Spell::Busy,
            spell_t: 0.0,
            tokens: 0,
            quiet: 0.0,
            header: Vec::new(),
            header_t: 0.0,
            headers: 0,
            timer: 0,
            fg: palette.fg,
            bg: palette.bg,
            history: [0; HISTORY],
            newest: 0,
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.spell == Spell::Busy
    }

    /// Paced tokens that arrived during the last tick.
    pub(super) fn fresh(&self) -> u32 {
        self.history[self.newest]
    }

    /// Paced tokens that arrived in the last `window` seconds, at most the stage's history.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "A time window deliberately selects whole history ticks; negative windows yield zero and large windows are capped at HISTORY."
    )]
    pub(super) fn tokens_in(&self, window: f64) -> f64 {
        let ticks = ((window / TICK).round() as usize).min(HISTORY);
        (0..ticks)
            .map(|back| f64::from(self.history[(self.newest + HISTORY - back) % HISTORY]))
            .sum()
    }

    /// Paced tokens per second over the last `window` seconds.
    pub(super) fn flow(&self, window: f64) -> f64 {
        self.tokens_in(window) / window
    }

    /// The terminal's foreground laid over its background at `alpha`, as codex-tui greys text.
    pub(super) fn grey(&self, alpha: f64) -> Rgb {
        self.fg.blend(self.bg, alpha)
    }

    /// One ink per header grapheme, from its index and text.
    pub(super) fn letters(&self, mut paint: impl FnMut(usize, &str) -> Ink) -> Vec<Ink> {
        self.header
            .iter()
            .enumerate()
            .map(|(i, letter)| paint(i, letter))
            .collect()
    }

    /// Moves the clock one tick, in which `tokens` paced tokens arrived during `spell`.
    pub(super) fn advance(&mut self, tokens: u32, spell: Spell) {
        self.spell_t = if spell != self.spell && self.t > 0.0 {
            0.0
        } else {
            self.spell_t + TICK
        };
        self.spell = spell;
        self.t += TICK;
        self.header_t += TICK;
        self.tokens += u64::from(tokens);
        self.quiet = if tokens > 0 { 0.0 } else { self.quiet + TICK };
        self.newest = (self.newest + 1) % HISTORY;
        self.history[self.newest] = tokens;
    }

    /// Shows `header` on the status row, restarting its age when it changes.
    pub(super) fn show(&mut self, header: &str) {
        use unicode_segmentation::UnicodeSegmentation;
        if !self
            .header
            .iter()
            .map(String::as_str)
            .eq(header.graphemes(/*is_extended*/ true))
        {
            self.header = header
                .graphemes(/*is_extended*/ true)
                .map(str::to_owned)
                .collect();
            self.header_t = 0.0;
            self.headers += 1;
        }
    }
}

/// The terminal's default colors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Palette {
    pub(super) fg: Rgb,
    pub(super) bg: Rgb,
}

/// A 24-bit color whose channels stay fractional until they reach the terminal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Rgb(pub(super) f64, pub(super) f64, pub(super) f64);

impl Rgb {
    /// This color laid over `under` at `alpha`.
    pub(super) fn blend(self, under: Rgb, alpha: f64) -> Rgb {
        let mix = |a: f64, b: f64| a * alpha + b * (1.0 - alpha);
        Rgb(
            mix(self.0, under.0),
            mix(self.1, under.1),
            mix(self.2, under.2),
        )
    }
}

impl From<(u8, u8, u8)> for Rgb {
    fn from((r, g, b): (u8, u8, u8)) -> Self {
        Rgb(f64::from(r), f64::from(g), f64::from(b))
    }
}

/// One cell's paint. Unset colors are the terminal's defaults; an unset glyph keeps the text beneath.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Ink {
    pub(super) glyph: Option<char>,
    pub(super) fg: Option<Rgb>,
    pub(super) bg: Option<Rgb>,
    pub(super) bold: bool,
}

impl Ink {
    pub(super) const fn of(glyph: char) -> Ink {
        Ink {
            glyph: Some(glyph),
            fg: None,
            bg: None,
            bold: false,
        }
    }

    pub(super) const fn fg(self, fg: Rgb) -> Ink {
        Ink {
            fg: Some(fg),
            ..self
        }
    }

    pub(super) const fn bg(self, bg: Rgb) -> Ink {
        Ink {
            bg: Some(bg),
            ..self
        }
    }

    pub(super) const fn bold(self, bold: bool) -> Ink {
        Ink { bold, ..self }
    }
}

/// The status row as a shimmer paints it.
#[derive(Default)]
pub(super) struct Row {
    /// The bullet, one ink per cell, each with a glyph.
    pub(super) mark: Vec<Ink>,
    /// One ink per header grapheme; a glyph stands in for its letter.
    pub(super) header: Vec<Ink>,
    /// Cells between the header and the timer, each with a glyph.
    pub(super) tail: Vec<Ink>,
    /// Backgrounds laid under the row's cells from its first column, whatever text lands there.
    pub(super) backdrop: Vec<Rgb>,
}
