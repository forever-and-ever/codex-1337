//! One issue body, cached at the current width. Selection changes discard the
//! cache; browsing never injects issue content into the conversation.

use super::jira_view::JiraBrowser;
use codex_app_server_protocol::JiraIssue;
use codex_app_server_protocol::JiraKey;
use codex_app_server_protocol::JiraResult;
use codex_app_server_protocol::JiraRevision;
use codex_app_server_protocol::ThreadJiraResponse;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

#[derive(Default)]
pub(super) struct JiraPreview {
    pub(super) result: Option<Result<JiraIssue, String>>,
    scroll: usize,
    markdown: Option<(JiraRevision, u16, Vec<Line<'static>>)>,
}

impl JiraBrowser {
    pub(super) fn finish_preview(
        &mut self,
        key: &JiraKey,
        result: Result<ThreadJiraResponse, String>,
    ) {
        if self.selected.as_ref() != Some(key) {
            return;
        }
        self.preview.result = Some(result.and_then(|response| {
            if response.project.as_str() != self.project
                || self.scope.as_ref() != Some(&response.scope)
            {
                return Err("Conversation project changed; close and reopen /jira.".into());
            }
            match response.result {
                JiraResult::Issue { issue } if &issue.revision.key == key => Ok(issue),
                _ => Err("Unexpected Jira preview response; close and reopen /jira.".into()),
            }
        }));
    }
}

impl JiraPreview {
    pub(super) fn issue(&self) -> Option<&JiraIssue> {
        self.result.as_ref()?.as_ref().ok()
    }

    pub(super) fn scroll(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
    }

    pub(super) fn render(&mut self, title: &str, area: Rect, buf: &mut Buffer) {
        let headings: Vec<_> = textwrap::wrap(title, area.width as usize)
            .into_iter()
            .map(|line| Line::from(line.into_owned()).bold())
            .collect();
        let heading_height = headings.len().min(area.height as usize) as u16;
        Paragraph::new(headings).render(
            Rect {
                height: heading_height,
                ..area
            },
            buf,
        );
        let offset = (heading_height + 1).min(area.height);
        let body = Rect::new(area.x, area.y + offset, area.width, area.height - offset);
        let lines = match &self.result {
            Some(Ok(issue)) => {
                if self.markdown.as_ref().is_some_and(|(revision, width, _)| {
                    revision != &issue.revision || *width != body.width
                }) {
                    self.markdown = None;
                }
                let (_, _, lines) = self.markdown.get_or_insert_with(|| {
                    let mut lines = Vec::new();
                    crate::markdown::append_markdown(
                        &issue.body,
                        Some(body.width as usize),
                        /*cwd*/ None,
                        &mut lines,
                    );
                    (issue.revision.clone(), body.width, lines)
                });
                lines
            }
            result => {
                let text = match result {
                    Some(Err(error)) => error.as_str(),
                    None => "Loading…",
                    Some(Ok(_)) => unreachable!(),
                };
                Paragraph::new(
                    textwrap::wrap(text, body.width as usize)
                        .into_iter()
                        .map(|line| Line::from(line.into_owned()).dim())
                        .collect::<Vec<_>>(),
                )
                .render(body, buf);
                return;
            }
        };
        self.scroll = self
            .scroll
            .min(lines.len().saturating_sub(body.height as usize));
        for (row, line) in lines
            .iter()
            .skip(self.scroll)
            .take(body.height as usize)
            .enumerate()
        {
            Paragraph::new(line.clone()).render(
                Rect::new(body.x, body.y + row as u16, body.width, /*height*/ 1),
                buf,
            );
        }
    }
}
