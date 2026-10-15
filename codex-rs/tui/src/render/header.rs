use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

pub(crate) fn style() -> Style {
    Style::default()
        .fg(crate::style::readable_color_on(
            Color::Rgb(255, 255, 255),
            None,
        ))
        .add_modifier(Modifier::BOLD)
        .remove_modifier(Modifier::DIM)
}

pub(crate) fn leading(label: &'static str, mut tail: Line<'static>) -> Line<'static> {
    tail.spans.insert(0, Span::styled(label, style()));
    tail
}

pub(crate) fn paint(mut line: Line<'static>) -> Line<'static> {
    // A line-wide foreground is an explicit semantic header color.
    let mut heading = style();
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
