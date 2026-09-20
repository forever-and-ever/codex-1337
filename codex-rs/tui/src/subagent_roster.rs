//! Live, root-scoped subagent presentation. Focus is independent of the watched thread.
use crate::agent_status;
use crate::line_truncation::truncate_line_to_width;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::render::renderable::Renderable;
use crate::status::format_tokens_compact;
use codex_app_server_protocol::ThreadStatus;
use codex_protocol::ThreadId;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

pub(crate) type SharedRoster = Arc<Mutex<SubagentRoster>>;

#[derive(Debug)]
pub(crate) struct AgentRow {
    pub id: ThreadId,
    pub name: String,
    pub status: Option<ThreadStatus>,
    pub activity: Option<String>,
    pub tokens: Option<i64>,
}

#[derive(Debug, Default)]
pub(crate) struct SubagentRoster {
    pub rows: Vec<AgentRow>,
    pub watched: Option<ThreadId>,
    pub focused: Option<ThreadId>,
    first: usize,
    name_width: usize,
}

impl SubagentRoster {
    pub fn reconcile(&mut self, agents: Vec<(ThreadId, String)>, watched: Option<ThreadId>) {
        let old_index = self
            .focused
            .and_then(|id| self.rows.iter().position(|row| row.id == id));
        let mut previous: HashMap<_, _> = self.rows.drain(..).map(|row| (row.id, row)).collect();
        self.rows = agents
            .into_iter()
            .map(|(id, name)| match previous.remove(&id) {
                Some(mut row) => {
                    row.name = name;
                    row
                }
                None => AgentRow {
                    id,
                    name,
                    status: None,
                    activity: None,
                    tokens: None,
                },
            })
            .collect();
        self.sort_by_activity();
        self.name_width = self
            .rows
            .iter()
            .map(|row| Line::from(row.name.as_str()).width())
            .max()
            .unwrap_or(0)
            .min(24);
        self.watched = watched;
        if self
            .focused
            .is_some_and(|id| !self.rows.iter().any(|row| row.id == id))
        {
            self.focused = old_index
                .and_then(|index| self.rows.get(index.min(self.rows.len().saturating_sub(1))))
                .map(|row| row.id);
        }
    }

    pub fn sort_by_activity(&mut self) {
        self.rows.sort_by(|a, b| {
            matches!(b.status, Some(ThreadStatus::Active { .. }))
                .cmp(&matches!(a.status, Some(ThreadStatus::Active { .. })))
                .then_with(|| a.name.cmp(&b.name))
        });
    }

    pub fn down(&mut self) {
        let index = self
            .focused
            .and_then(|id| self.rows.iter().position(|row| row.id == id))
            .map_or(0, |index| {
                (index + 1).min(self.rows.len().saturating_sub(1))
            });
        self.focused = self.rows.get(index).map(|row| row.id);
    }

    pub fn up(&mut self) {
        self.focused = self
            .focused
            .and_then(|id| self.rows.iter().position(|row| row.id == id))
            .and_then(|index| index.checked_sub(1))
            .map(|index| self.rows[index].id);
    }
}

pub(crate) struct RosterView(pub SharedRoster);

impl Renderable for RosterView {
    fn desired_height(&self, _width: u16) -> u16 {
        let roster = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        roster.rows.len().min(6) as u16
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let area = area.inset(Insets::tlbr(
            /*top*/ 0, /*left*/ 0, /*bottom*/ 0, /*right*/ 2,
        ));
        if area.is_empty() {
            return;
        }
        let mut roster = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let height = usize::from(area.height);
        let index = roster
            .focused
            .or(roster.watched)
            .and_then(|id| roster.rows.iter().position(|row| row.id == id));
        if let Some(index) = index {
            roster.first = roster
                .first
                .min(index)
                .max(index.saturating_sub(height - 1));
        }
        roster.first = roster.first.min(roster.rows.len().saturating_sub(height));
        let name_width = roster.name_width.min(usize::from(area.width) / 3);
        for (offset, row) in roster
            .rows
            .iter()
            .skip(roster.first)
            .take(height)
            .enumerate()
        {
            let tokens = row
                .tokens
                .map_or_else(|| "—".to_string(), format_tokens_compact)
                .dim();
            let tokens_width = tokens.width().min(usize::from(area.width));
            let edge = if offset == 0 && roster.first > 0 {
                '↑'
            } else if offset + 1 == height && roster.first + height < roster.rows.len() {
                '↓'
            } else {
                ' '
            };
            let mut spans = vec![
                format!(
                    "{}{edge}",
                    if roster.focused == Some(row.id) {
                        '›'
                    } else {
                        ' '
                    }
                )
                .into(),
                agent_status::marker(row.status.as_ref()),
                " ".into(),
            ];
            let name = truncate_line_to_width(Line::from(row.name.clone()), name_width);
            let padding = name_width.saturating_sub(name.width()) + 2;
            spans.extend(name.spans);
            spans.push(" ".repeat(padding).into());
            spans.push(row.activity.as_deref().unwrap_or("—").to_string().dim());
            let width = usize::from(area.width).saturating_sub(tokens_width + 1);
            let mut line = Line::from(spans);
            if line.width() > width && width > 0 {
                line = truncate_line_to_width(line, width - 1);
                line.spans.push("…".dim());
            } else {
                line = truncate_line_to_width(line, width);
            }
            line.spans.push(
                " ".repeat(usize::from(area.width).saturating_sub(line.width() + tokens_width))
                    .into(),
            );
            line.spans.push(tokens);
            if roster.watched == Some(row.id) {
                line = line.reversed();
            }
            Paragraph::new(line).render(
                Rect {
                    y: area.y + offset as u16,
                    height: 1,
                    ..area
                },
                buf,
            );
        }
    }
}
