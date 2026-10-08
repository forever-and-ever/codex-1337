//! Prompt-cache glyphs in the composer's bottom padding row: a radiator while warming is on,
//! a snowflake while the cache is probably cold. Material Design icons from a Nerd Font.
use super::ChatComposer;
use super::ComposerRenderOptions;
use crate::style::StatusTone;
use crate::style::status_style;
use crate::ui_consts::LIVE_PREFIX_COLS;
use chrono::Local;
use chrono::TimeZone;
use codex_app_server_protocol::PromptCacheEstimate;
use codex_app_server_protocol::PromptCacheWarming;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

const RADIATOR: &str = "\u{F0438}";
const SNOWFLAKE: &str = "\u{F0717}";

/// A conversation's prompt-cache state as the server last published it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PromptCacheGlyphs {
    pub(crate) estimate: PromptCacheEstimate,
    pub(crate) warming: PromptCacheWarming,
}

impl Default for PromptCacheGlyphs {
    fn default() -> Self {
        Self {
            estimate: PromptCacheEstimate::Unknown,
            warming: PromptCacheWarming::Off,
        }
    }
}

impl PromptCacheGlyphs {
    fn line(self, now: i64) -> Option<Line<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if let PromptCacheWarming::Armed { ends_at } | PromptCacheWarming::Holding { ends_at } =
            self.warming
        {
            spans.push(Span::styled(RADIATOR, status_style(StatusTone::Attention)));
            if let Some(ends) = Local.timestamp_opt(ends_at, /*nsecs*/ 0).single() {
                spans.push(format!(" until {}", ends.format("%H:%M")).dim());
            }
        }
        let cold = match self.estimate {
            PromptCacheEstimate::Unknown => false,
            PromptCacheEstimate::Warm { expires_at } => now >= expires_at,
            PromptCacheEstimate::Cold => true,
        };
        if cold {
            if !spans.is_empty() {
                spans.push(" ".into());
            }
            spans.push(SNOWFLAKE.cyan());
        }
        (!spans.is_empty()).then(|| Line::from(spans))
    }
}

impl ChatComposer {
    /// Right-aligned beside the ambient pet's reserve; skipped when the row would clip it.
    pub(super) fn render_prompt_cache_glyphs(
        &self,
        composer: Rect,
        options: ComposerRenderOptions<'_>,
        buf: &mut Buffer,
    ) {
        let Some(line) = options.prompt_cache.line(chrono::Utc::now().timestamp()) else {
            return;
        };
        let width = u16::try_from(line.width()).unwrap_or(u16::MAX);
        let right = composer
            .right()
            .saturating_sub(1)
            .saturating_sub(options.textarea_right_reserve);
        let left = right.saturating_sub(width);
        if composer.height < 3 || left <= composer.x.saturating_add(LIVE_PREFIX_COLS) {
            return;
        }
        buf.set_line(left, composer.bottom() - 1, &line, width);
    }
}

#[cfg(test)]
#[path = "prompt_cache_glyphs_tests.rs"]
mod tests;
