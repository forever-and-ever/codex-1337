use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

#[expect(
    clippy::disallowed_methods,
    reason = "Fork header role colors are truecolor tokens contrast-corrected against the terminal background."
)]
pub(crate) fn style() -> Style {
    Style::default()
        .fg(crate::style::readable_color_on(
            Color::Rgb(255, 255, 255),
            None,
        ))
        .add_modifier(Modifier::BOLD)
        .remove_modifier(Modifier::DIM)
}

#[expect(
    clippy::disallowed_methods,
    reason = "The active-work role uses its established contrast-corrected fork palette token."
)]
pub(crate) fn work_style() -> Style {
    style().fg(crate::style::readable_color_on(
        Color::Rgb(194, 217, 139),
        None,
    ))
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
            span.style = span.style.patch(heading);
        }
    }
    line
}
