use ratatui::style::Modifier;
use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;

pub(crate) fn glyph(expanded: bool) -> &'static str {
    if expanded { "[-]" } else { "[+]" }
}

pub(crate) fn columns() -> usize {
    glyph(false).width()
}

pub(crate) fn style(focused: bool) -> Style {
    let style = Style::default()
        .fg(ratatui::style::Color::Reset)
        .remove_modifier(Modifier::BOLD | Modifier::DIM);
    if focused {
        style.add_modifier(Modifier::UNDERLINED)
    } else {
        style
    }
}

pub(crate) fn tab_style() -> Style {
    super::header::style().add_modifier(Modifier::UNDERLINED)
}
