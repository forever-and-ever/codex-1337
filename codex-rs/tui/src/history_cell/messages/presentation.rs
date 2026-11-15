//! The fullscreen transcript omits speaker markers; legacy inline scrollback retains them.

use super::AgentMarkdownCell;
use super::AgentMessageCell;
use super::UserHistoryCell;
use super::UserMessagePresentation;
use super::normalize_whitespace_only_hyperlink_lines;
use super::sanitize_user_text;
use super::spoken;
use super::wrap_user_message;
use crate::history_cell::HistoryCell;
use crate::history_cell::agent_prefix;
use crate::markdown_render::ListSpacing;
use crate::style::accent_color_on;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;
use crate::warpaint::Bracket;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use ratatui::style::Style;
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
    let message = if cell.is_spoken() {
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
    let style = Style::default();
    let element_style = style.fg(accent_color_on(style.bg));
    let mut lines = plain_hyperlink_lines(adaptive_wrap_lines(
        cell.image_labels_not_in_message()
            .map(|label| Line::from(label).style(element_style)),
        RtOptions::new(usize::from(wrap_width)).wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
    ));
    if surface == Surface::Inline {
        lines = prefix_hyperlink_lines(lines, "  ".into(), "  ".into());
    }
    let replies = matches!(
        cell.presentation,
        UserMessagePresentation::AsyncReplies { .. }
    );
    let mut copy_as_prose = !replies;
    let mut wrapped_message = match &cell.presentation {
        UserMessagePresentation::AsyncReplies { replies, cwd } => {
            super::super::async_replies::render(replies, cwd, usize::from(wrap_width))
        }
        UserMessagePresentation::Typed | UserMessagePresentation::Spoken => {
            let markdown = if cell.uses_markdown() {
                crate::markdown::render_markdown_for_viewport(message, wrap_width)
            } else {
                Vec::new()
            };
            copy_as_prose = !crate::markdown_copy::has_visible_markup(&markdown, message);
            if copy_as_prose {
                wrap_user_message(message, text_elements, style, wrap_width)
            } else {
                markdown
            }
        }
    };
    if !wrapped_message.is_empty() {
        if !lines.is_empty() {
            lines.push(HyperlinkLine::new(Line::from("").style(style)));
        }
        if surface == Surface::Inline && !replies {
            wrapped_message = prefix_hyperlink_lines(
                wrapped_message,
                if cell.is_spoken() {
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
            source.copy_as_prose |= copy_as_prose;
        }
    }
    spoken(lines, Bracket::Prompt)
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
    spoken(wrapped, Bracket::Response)
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
            return spoken(
                prefix_hyperlink_lines(
                    vec![HyperlinkLine::new(Line::default())],
                    agent_prefix(true),
                    agent_prefix(false),
                ),
                Bracket::Response,
            );
        };
        let markdown = |source: &str| {
            crate::markdown::render_markdown_agent_with_list_spacing(
                source,
                Some(wrap_width),
                Some(cell.cwd.as_path()),
                cell.inline_visualization_context.as_ref(),
                list_spacing,
            )
        };
        let lines = if cell.question_titles.is_empty() {
            let mut lines = markdown(&cell.markdown_source);
            if cell.spoken_artifacts {
                super::super::spoken_artifacts::annotate_spoken_artifacts(&mut lines, &cell.cwd);
            }
            if surface == Surface::Inline {
                lines = prefix_hyperlink_lines(lines, agent_prefix(true), agent_prefix(false));
            }
            lines
        } else {
            super::super::async_questions::render(
                &cell.markdown_source,
                &cell.question_titles,
                markdown,
            )
        };
        spoken(
            normalize_whitespace_only_hyperlink_lines(lines),
            Bracket::Response,
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

// TELOS(fast-ui-thread): parked stream handoff carries raw source; its worker strips directives.
