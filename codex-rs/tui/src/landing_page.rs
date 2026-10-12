//! Client-only empty-space decoration, never conversation history or model input.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

use crate::style::accent_style;
use crate::style::secondary_text_style;

pub(crate) fn render(available: Rect, buffer: &mut Buffer) {
    let area = available.intersection(buffer.area);
    if area.width < 7 || area.height < 4 {
        return;
    }
    let mut lines = if area.width >= 48 && area.height >= 10 {
        [
            "▄█   ▀▀▀█ ▀▀▀█ ▀▀▀▀█",
            " █    ▀▀█  ▀▀█    █ ",
            " █      █    █   █  ",
            "▄█▄  ▀▀▀▀ ▀▀▀▀  █   ",
        ]
        .into_iter()
        .map(|text| Line::styled(text, accent_style()))
        .collect::<Vec<_>>()
    } else {
        vec![Line::styled("1337", accent_style())]
    };
    lines.push(Line::default());
    for (command, description) in [
        ("/resume", "Resume a saved chat"),
        ("/model", "Model and reasoning"),
    ] {
        let mut spans = vec![Span::styled(command, Style::default().bold())];
        if area.width >= 32 {
            spans.push(Span::styled(
                format!("{:padding$}{description}", "", padding = 11 - command.len()),
                secondary_text_style(),
            ));
        }
        lines.push(Line::from(spans));
    }
    let width = u16::try_from(lines.iter().map(Line::width).max().unwrap_or_default())
        .expect("landing page width fits u16")
        .min(area.width);
    let height = u16::try_from(lines.len()).expect("landing page has at most seven rows");
    let x = area.x + (area.width - width) / 2;
    let y = area.y + (area.height - height) / 2;
    for (offset, line) in (0..height).zip(lines) {
        line.render(Rect::new(x, y + offset, width, 1), buffer);
    }
}
