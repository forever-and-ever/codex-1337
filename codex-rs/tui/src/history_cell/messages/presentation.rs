//! The fullscreen transcript omits speaker markers; legacy inline scrollback retains them.

use super::AgentMarkdownCell;
use super::AgentMessageCell;
use super::UserHistoryCell;
use super::message_band;
use super::normalize_whitespace_only_hyperlink_lines;
use super::sanitize_user_text;
use super::wrap_user_message;
use crate::history_cell::HistoryCell;
use crate::history_cell::agent_prefix;
use crate::markdown_render::ListSpacing;
use crate::style::accent_color_on;
use crate::style::history_prompt_style;
use crate::style::history_response_style;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use ratatui::style::Stylize;
use ratatui::text::Line;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Surface {
    Inline,
    Transcript,
}

impl Surface {
    fn marker_columns(self) -> u16 {
        match self {
            Self::Inline => 2,
            Self::Transcript => 0,
        }
    }

    fn list_spacing(self) -> ListSpacing {
        match self {
            Self::Inline => ListSpacing::AfterMultiline,
            Self::Transcript => ListSpacing::Uniform,
        }
    }
}

pub(super) fn user_lines(
    cell: &UserHistoryCell,
    width: u16,
    surface: Surface,
) -> Vec<HyperlinkLine> {
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
    let wrap_width = width.saturating_sub(surface.marker_columns() + 1).max(1);
    let style = history_prompt_style();
    let element_style = style.fg(accent_color_on(style.bg));
    let mut lines = plain_hyperlink_lines(adaptive_wrap_lines(
        cell.image_labels_not_in_message()
            .map(|label| Line::from(label).style(element_style)),
        RtOptions::new(usize::from(wrap_width)).wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
    ));
    if surface == Surface::Inline {
        lines = prefix_hyperlink_lines(lines, "  ".into(), "  ".into());
    }
    if let Some(mut wrapped_message) = wrap_user_message(message, text_elements, style, wrap_width)
    {
        if !lines.is_empty() {
            lines.push(HyperlinkLine::new(Line::from("").style(style)));
        }
        if surface == Surface::Inline {
            wrapped_message = prefix_hyperlink_lines(
                wrapped_message,
                if cell.spoken {
                    "› ".red().bold()
                } else {
                    "› ".bold().dim()
                },
                "  ".into(),
            );
        }
        lines.extend(wrapped_message);
    }
    for line in &mut lines {
        if line.source.is_none() && line.width() > 0 {
            line.source = Some(crate::terminal_hyperlinks::LogicalLineSource::from_line(
                &line.line,
            ));
        }
        if let Some(source) = &mut line.source {
            source.right_reserve = 1;
            source.copy_as_prose = true;
        }
    }
    message_band(lines, style)
}

pub(super) fn agent_lines(
    cell: &AgentMessageCell,
    width: u16,
    surface: Surface,
) -> Vec<HyperlinkLine> {
    let mut wrapped = Vec::new();
    for (index, line) in cell.lines.iter().enumerate() {
        let initial = if surface == Surface::Inline {
            agent_prefix(index == 0 && cell.is_first_line).into()
        } else {
            Line::default()
        };
        let mut subsequent = if surface == Surface::Inline {
            Line::from(agent_prefix(false))
        } else {
            Line::default()
        };
        subsequent
            .spans
            .extend(crate::insert_history::leading_whitespace_prefix(&line.line).spans);
        wrapped.extend(crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
            std::slice::from_ref(line),
            RtOptions::new(usize::from(width))
                .initial_indent(initial)
                .subsequent_indent(subsequent),
        ));
    }
    message_band(wrapped, history_response_style())
}

pub(super) fn markdown_lines(
    cell: &AgentMarkdownCell,
    width: u16,
    surface: Surface,
) -> Vec<HyperlinkLine> {
    let list_spacing = surface.list_spacing();
    let render = || {
        let Some(wrap_width) =
            crate::width::usable_content_width_u16(width, surface.marker_columns())
        else {
            return message_band(
                prefix_hyperlink_lines(
                    vec![HyperlinkLine::new(Line::default())],
                    agent_prefix(true),
                    agent_prefix(false),
                ),
                history_response_style(),
            );
        };
        let mut lines = crate::markdown::render_markdown_agent_with_list_spacing(
            &cell.markdown_source,
            Some(wrap_width),
            Some(cell.cwd.as_path()),
            cell.inline_visualization_context.as_ref(),
            list_spacing,
        );
        if cell.spoken_artifacts {
            super::super::spoken_artifacts::annotate_spoken_artifacts(&mut lines, &cell.cwd);
        }
        if surface == Surface::Inline {
            lines = prefix_hyperlink_lines(lines, agent_prefix(true), agent_prefix(false));
        }
        message_band(
            normalize_whitespace_only_hyperlink_lines(lines),
            history_response_style(),
        )
    };
    // The surfaces use different list-spacing keys as well as different markers.
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
