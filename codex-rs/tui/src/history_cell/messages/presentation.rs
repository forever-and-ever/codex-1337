//! Message bodies share the feed-title column; only fallible activities carry markers.

use super::AgentMarkdownCell;
use super::AgentMessageCell;
use super::UserHistoryCell;
use super::message_band;
use super::normalize_whitespace_only_hyperlink_lines;
use super::sanitize_user_text;
use super::wrap_user_message;
use crate::history_cell::HistoryCell;
use crate::style::accent_color_on;
use crate::style::history_prompt_style;
use crate::style::history_response_style;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use ratatui::text::Line;

pub(super) fn user_lines(cell: &UserHistoryCell, width: u16) -> Vec<HyperlinkLine> {
    let sanitized_message = sanitize_user_text((&cell.message).into());
    let message = if cell.spoken {
        sanitized_message.trim_start()
    } else {
        sanitized_message.as_ref()
    };
    let text_elements = if message == cell.message {
        cell.text_elements.as_slice()
    } else {
        &[]
    };
    let wrap_width = width.saturating_sub(1).max(1);
    let style = history_prompt_style();
    let element_style = style.fg(accent_color_on(style.bg));
    let wrapped_images = plain_hyperlink_lines(adaptive_wrap_lines(
        cell.image_labels_not_in_message()
            .map(|label| Line::from(label).style(element_style)),
        RtOptions::new(usize::from(wrap_width)).wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
    ));
    let wrapped_message = wrap_user_message(message, text_elements, style, wrap_width);
    let mut lines = wrapped_images;
    if let Some(wrapped_message) = wrapped_message {
        if !lines.is_empty() {
            lines.push(HyperlinkLine::new(Line::from("").style(style)));
        }
        lines.extend(wrapped_message);
    }
    for source in lines.iter_mut().filter_map(|line| line.source.as_mut()) {
        source.right_reserve = 1;
        source.copy_as_prose = true;
    }
    message_band(lines, style)
}

pub(super) fn agent_lines(cell: &AgentMessageCell, width: u16) -> Vec<HyperlinkLine> {
    let mut wrapped = Vec::new();
    for line in &cell.lines {
        wrapped.extend(crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
            std::slice::from_ref(line),
            RtOptions::new(usize::from(width))
                .subsequent_indent(crate::insert_history::leading_whitespace_prefix(&line.line)),
        ));
    }
    message_band(wrapped, history_response_style())
}

pub(super) fn markdown_lines(
    cell: &AgentMarkdownCell,
    width: u16,
    list_spacing: crate::markdown_render::ListSpacing,
) -> Vec<HyperlinkLine> {
    let render = || {
        let mut lines = crate::markdown::render_markdown_agent_with_list_spacing(
            &cell.markdown_source,
            Some(usize::from(width.max(1))),
            Some(cell.cwd.as_path()),
            cell.inline_visualization_context.as_ref(),
            list_spacing,
        );
        if cell.spoken_artifacts {
            super::super::spoken_artifacts::annotate_spoken_artifacts(&mut lines, &cell.cwd);
        }
        message_band(
            normalize_whitespace_only_hyperlink_lines(lines),
            history_response_style(),
        )
    };
    if let Some(rendered_lines) = &cell.rendered_lines {
        rendered_lines.render(width, list_spacing, render)
    } else {
        render()
    }
}

pub(crate) fn is_message(cell: &dyn HistoryCell) -> bool {
    cell.as_any().is::<UserHistoryCell>()
        || cell.as_any().is::<AgentMessageCell>()
        || cell.as_any().is::<AgentMarkdownCell>()
}
