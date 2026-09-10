//! Read-only child presentation in place of the editable prompt.
use ratatui::style::Stylize;
use ratatui::text::Line;

pub(super) fn lines(width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    textwrap::wrap(
        "Viewing sub-agent — direct input is disabled · Esc main · Ctrl+C interrupt",
        textwrap::Options::new(usize::from(width))
            .initial_indent("  ")
            .subsequent_indent("  "),
    )
    .into_iter()
    .map(|line| Line::from(line.into_owned()).dim())
    .collect()
}
