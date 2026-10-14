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
    let style = super::header::style();
    if focused {
        style.add_modifier(Modifier::UNDERLINED)
    } else {
        style
    }
}
