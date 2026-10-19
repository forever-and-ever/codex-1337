//! A shimmer on its own clock: stepped one tick at a time, its paint turned into spans.

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Span;

use super::ROSTER;
use super::stage::Ink;
use super::stage::Palette;
use super::stage::Rgb;
use super::stage::Row;
use super::stage::Shimmer;
use super::stage::Spell;
use super::stage::Stage;
use crate::terminal_palette::rgb_color;

pub(super) struct Engine {
    /// The roster entry this runs.
    pub(super) recipe: usize,
    shimmer: Box<dyn Shimmer>,
    stage: Stage,
}

impl Engine {
    pub(super) fn new(recipe: usize, palette: Palette) -> Self {
        Self {
            recipe,
            shimmer: (ROSTER[recipe].recipe.summon)(),
            stage: Stage::new(palette),
        }
    }

    /// Advances one tick in which `tokens` arrived during `spell`.
    pub(super) fn step(&mut self, tokens: u32, spell: Spell) {
        self.stage.advance(tokens, spell);
        self.shimmer.tick(&self.stage);
    }

    /// The status row's lead (mark, header and tail) and its backdrop. A still mark is dropped and
    /// still letters are plain, as `tui.effects` asks; the backdrop belongs to the letters.
    pub(super) fn status(
        &mut self,
        header: &str,
        timer: u64,
        palette: Palette,
        mark: bool,
        letters: bool,
    ) -> (Vec<Span<'static>>, Vec<Rgb>) {
        self.stage.show(header);
        self.stage.timer = timer;
        self.paint_with(palette);
        let mut row = Row {
            header: vec![Ink::default(); self.stage.header.len()],
            ..Row::default()
        };
        self.shimmer.status(&self.stage, &mut row);
        let mut spans = Vec::new();
        if mark && !row.mark.is_empty() {
            spans.extend(row.mark.iter().map(|ink| ink.span("•")));
            spans.push(" ".into());
        }
        if !letters {
            spans.push(header.to_owned().into());
            return (spans, Vec::new());
        }
        for i in 0..=self.stage.header.len() {
            if let Some((_, ink)) = row.header_insert.filter(|(at, _)| *at == i) {
                spans.push(ink.span(" "));
            }
            if let Some(letter) = self.stage.header.get(i) {
                spans.push(row.header.get(i).copied().unwrap_or_default().span(letter));
            }
        }
        spans.extend(row.tail.iter().map(|ink| ink.span(" ")));
        (spans, row.backdrop)
    }

    /// A running tool's bullet.
    pub(super) fn tool(&mut self, palette: Palette) -> Span<'static> {
        self.paint_with(palette);
        self.shimmer.tool(&self.stage).span("•")
    }

    fn paint_with(&mut self, palette: Palette) {
        self.stage.fg = palette.fg;
        self.stage.bg = palette.bg;
    }
}

impl Ink {
    /// This ink over `text`, unless it carries its own glyph.
    fn span(self, text: &str) -> Span<'static> {
        let mut style = Style::default();
        if let Some(fg) = self.fg {
            style = style.fg(color(fg));
        }
        if let Some(bg) = self.bg {
            style = style.bg(color(bg));
        }
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        Span::styled(
            self.glyph.map_or_else(|| text.to_owned(), String::from),
            style,
        )
    }
}

/// The terminal color for `rgb`, truncating channels as codex-tui's `blend` does.
pub(super) fn color(rgb: Rgb) -> Color {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Terminal RGB quantization deliberately truncates channels clamped to 0..=255."
    )]
    let channel = |v: f64| v.clamp(0.0, 255.0) as u8;
    rgb_color((channel(rgb.0), channel(rgb.1), channel(rgb.2)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shimmers::ROSTER;
    use crate::shimmers::script::Script;
    use crate::shimmers::stage::TICK;

    #[test]
    fn every_stock_recipe_renders_a_full_script() {
        let palette = Palette {
            fg: Rgb(208.0, 208.0, 208.0),
            bg: Rgb(21.0, 21.0, 21.0),
        };
        let script = Script::new();
        for recipe in 0..ROSTER.len() {
            let mut engine = Engine::new(recipe, palette);
            for tick in 1..=540_u32 {
                let t = f64::from(tick) * TICK;
                let (spell, header, tokens) = script.at(t);
                let (_, _, before) = script.at(t - TICK);
                engine.step(u32::try_from(tokens - before).unwrap_or(u32::MAX), spell);
                let (spans, backdrop) = engine.status(
                    header,
                    63 + u64::from(tick),
                    palette,
                    /*mark*/ true,
                    /*letters*/ true,
                );
                assert!(!spans.is_empty(), "{recipe} rendered no status");
                assert!(backdrop.len() <= 80, "{recipe} overflowed the row");
                engine.tool(palette);
            }
        }
    }

    #[test]
    fn tectonic_preserves_graphemes_across_wrapped_faults() {
        use unicode_segmentation::UnicodeSegmentation;
        use unicode_width::UnicodeWidthStr;

        let palette = Palette {
            fg: Rgb(208.0, 208.0, 208.0),
            bg: Rgb(21.0, 21.0, 21.0),
        };
        let recipe = ROSTER
            .iter()
            .position(|entry| entry.recipe.name == "Tectonic")
            .unwrap();
        for header in ["", "X", "Working", "e\u{301}界👩‍💻ing"] {
            let mut engine = Engine::new(recipe, palette);
            engine.stage.show(header);
            let gaps = header.graphemes(true).count().saturating_sub(1);
            let mut previous = None;
            let mut visited = 0_u64;
            for _ in 0..256 {
                // Enough paced tokens for one quake per tick, without changing its production cadence.
                engine.step(240, Spell::Busy);
                let (spans, _) = engine.status(header, 0, palette, false, true);
                let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
                assert_eq!(text.replace('│', ""), header);
                assert_eq!(
                    spans.iter().map(Span::width).sum::<usize>(),
                    header.width() + usize::from(gaps > 0)
                );
                let fault = spans.iter().position(|span| span.content == "│");
                if gaps == 0 {
                    assert_eq!(fault, None);
                    continue;
                }
                let fault = fault.unwrap() - 1;
                assert!(fault < gaps);
                if let Some(previous) = previous {
                    assert!((previous + 1) % gaps == fault || (fault + 1) % gaps == previous);
                }
                previous = Some(fault);
                visited |= 1 << fault;
            }
            if gaps > 2 {
                assert!(visited.count_ones() > 2, "fault stayed near the centre");
            }
            for (spell, letters) in [(Spell::Wait, true), (Spell::Busy, false)] {
                engine.step(0, spell);
                let (spans, _) = engine.status(header, 0, palette, false, letters);
                let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
                assert_eq!(text, header);
            }
        }
    }
}
