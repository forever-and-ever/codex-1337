//! Display-only token figures. Accounting remains integer-valued upstream.

use crate::color::blend;
use crate::terminal_palette::best_color;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::default_fg;
use crate::terminal_palette::rgb_color;
use ratatui::style::Style;
use ratatui::text::Span;

/// A loose row's tokens, right-aligned in the gutter.
pub(super) fn token_span(tokens: u64) -> Span<'static> {
    token_figure(
        token_label(tokens),
        tokens,
        usize::from(crate::transcript_geometry::TOKEN_COLUMNS),
    )
}

/// A right-aligned figure, `label`, colored by the size of the tokens it reports.
pub(in crate::transcript_view) fn token_figure(
    label: String,
    tokens: u64,
    width: usize,
) -> Span<'static> {
    let color = match tokens {
        0..=1_000 => {
            let alpha = if tokens <= 300 { 0.2 } else { 0.45 };
            // These two levels deliberately remain below informative-text contrast.
            best_color(blend(
                default_fg().unwrap_or((220, 220, 220)),
                default_bg().unwrap_or((0, 0, 0)),
                alpha,
            ))
        }
        1_001..=3_000 => {
            crate::style::readable_color_on(rgb_color((230, 190, 60)), /*background*/ None)
        }
        3_001..=10_000 => {
            crate::style::readable_color_on(rgb_color((230, 130, 50)), /*background*/ None)
        }
        10_001.. => {
            crate::style::readable_color_on(rgb_color((220, 65, 65)), /*background*/ None)
        }
    };
    Span::styled(format!("{label:>width$}"), Style::reset().fg(color))
}

/// Exact below 100,000; three significant digits above.
pub(in crate::transcript_view) fn token_label(tokens: u64) -> String {
    if tokens < 100_000 {
        return tokens.to_string();
    }
    // Round in u128 so even u64::MAX cannot overflow at the halfway increment.
    let mut exponent = tokens.ilog10() - 2;
    let divisor = 10u128.pow(exponent);
    let mut significant = (u128::from(tokens) + divisor / 2) / divisor;
    if significant == 1_000 {
        significant = 100;
        exponent += 1;
    }
    let unit = exponent.div_ceil(/*rhs*/ 3);
    let decimals = unit * 3 - exponent;
    let scale = 10u128.pow(decimals);
    let whole = significant / scale;
    let suffix = ['K', 'M', 'G', 'T', 'P', 'E'][(unit - 1) as usize];
    if decimals == 0 {
        format!("{whole}{suffix}")
    } else {
        let fraction = significant % scale;
        let precision = decimals as usize;
        format!("{whole}.{fraction:0precision$}{suffix}")
    }
}
