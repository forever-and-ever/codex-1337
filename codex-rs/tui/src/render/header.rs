//! Feed headings: entry titles speak in the page's ink, bold, since a hue would claim a meaning
//! they lack, and a heading that names its speaker wears the speaker's bracket, the one meaning it
//! has. Active work speaks in its accent, the pack's signature, as heat does. An open tab
//! beneath them takes the pack's tab. Words that carry a meaning of their own keep its color: a
//! failure stays red in any heading.

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::warpaint::Bracket;
use crate::warpaint::Figure;

pub(crate) fn style() -> Style {
    Style::default()
        .fg(Figure::Ink.color())
        .add_modifier(Modifier::BOLD)
        .remove_modifier(Modifier::DIM)
}

pub(crate) fn work_style() -> Style {
    style().fg(Figure::Accent.color())
}

pub(crate) fn leading(label: &'static str, mut tail: Line<'static>) -> Line<'static> {
    tail.spans.insert(0, Span::styled(label, style()));
    tail
}

pub(crate) fn paint(line: Line<'static>) -> Line<'static> {
    paint_with(line, style())
}

pub(crate) fn paint_work(line: Line<'static>, active: bool) -> Line<'static> {
    paint_with(line, if active { work_style() } else { style() })
}

/// Paints a heading that names `speaker`, unless the work beneath it still runs.
pub(crate) fn paint_speaker(line: Line<'static>, speaker: Bracket, active: bool) -> Line<'static> {
    paint_with(
        line,
        if active {
            work_style()
        } else {
            style().fg(speaker.color())
        },
    )
}

fn paint_with(mut line: Line<'static>, mut heading: Style) -> Line<'static> {
    // A line-wide foreground is an explicit semantic header color.
    heading.fg = line.style.fg.or(heading.fg);
    for span in &mut line.spans {
        if line
            .style
            .patch(span.style)
            .add_modifier
            .contains(Modifier::BOLD)
        {
            let meant = span
                .style
                .fg
                .filter(|fg| !matches!(fg, Color::Reset | Color::White));
            span.style = span.style.patch(heading);
            span.style.fg = meant.or(span.style.fg);
        }
    }
    line
}
