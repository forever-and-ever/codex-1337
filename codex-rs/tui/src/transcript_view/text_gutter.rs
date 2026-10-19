//! Display-only token figures. Accounting remains integer-valued upstream.

use crate::warpaint::Figure;
use ratatui::style::Style;
use ratatui::text::Span;

/// A loose row's tokens, right-aligned in the gutter.
pub(super) fn token_span(tokens: u64) -> Span<'static> {
    token_figure(
        &token_label(tokens),
        tokens,
        usize::from(crate::transcript_geometry::TOKEN_COLUMNS),
    )
}

/// A right-aligned figure, `label`, painted by the size of the tokens it reports: small bills
/// recede into the faint and muted inks, ordinary ones read as body text, large ones as caution
/// and the largest as failure.
pub(in crate::transcript_view) fn token_figure(
    label: &str,
    tokens: u64,
    width: usize,
) -> Span<'static> {
    let figure = match tokens {
        0..=300 => Figure::InkFaint,
        301..=1_000 => Figure::InkMuted,
        1_001..=3_000 => Figure::Ink,
        3_001..=10_000 => Figure::Caution,
        10_001.. => Figure::Failure,
    };
    Span::styled(
        format!("{label:>width$}"),
        Style::reset().fg(figure.color()),
    )
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
