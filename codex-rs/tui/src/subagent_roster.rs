//! Inline view of loaded subagents. The picker retains unloaded family members.
use crate::agent_status;
use crate::bottom_pane::StatusLineItem;
use crate::bottom_pane::status_line_from_segments;
use crate::chatwidget::ChatWidget;
use crate::line_truncation::truncate_line_to_width;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::render::renderable::Renderable;
use crate::status::format_tokens_compact;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadStatus;
use codex_protocol::ThreadId;
use codex_protocol::openai_models::ReasoningEffort;
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
pub(crate) enum Retirement {
    Pending,
    Reconciling(ThreadId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentModel {
    pub model: String,
    pub effort: Option<ReasoningEffort>,
}

impl AgentModel {
    pub fn from_thread(thread: &Thread) -> Option<Self> {
        thread.model.as_ref().map(|model| Self {
            model: model.clone(),
            effort: thread.reasoning_effort.clone(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct AgentRow {
    pub id: ThreadId,
    pub name: String,
    pub status: Option<ThreadStatus>,
    pub activity: Option<String>,
    pub tokens: Option<i64>,
    pub model: Option<AgentModel>,
}

#[derive(Debug, Default)]
pub(crate) struct SubagentRoster {
    pub rows: Vec<AgentRow>,
    pub watched: Option<ThreadId>,
    pub focused: Option<ThreadId>,
    pub retiring: Option<Retirement>,
    first: usize,
}

impl SubagentRoster {
    pub fn reconcile(&mut self, agents: Vec<(ThreadId, String)>, watched: Option<ThreadId>) {
        let old_index = self
            .focused
            .and_then(|id| self.rows.iter().position(|row| row.id == id));
        let focused_order = self.focused.map(|_| {
            self.rows
                .iter()
                .enumerate()
                .map(|(index, row)| (row.id, index))
                .collect::<HashMap<_, _>>()
        });
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
                    model: None,
                },
            })
            .collect();
        if let Some(order) = focused_order {
            self.rows
                .sort_by_key(|row| order.get(&row.id).copied().unwrap_or(usize::MAX));
        }
        self.sort_by_activity();
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
        if self.focused.is_some() {
            return;
        }
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
        self.sort_by_activity();
    }
}

pub(crate) struct RosterView {
    pub roster: SharedRoster,
    pub use_theme_colors: bool,
}

impl RosterView {
    fn model_cell(&self, row: &AgentRow) -> Line<'static> {
        let Some(model) = &row.model else {
            return Line::default();
        };
        let mut label = model.model.clone();
        if let Some(effort) = &model.effort {
            label.push(' ');
            label.push_str(&ChatWidget::status_line_reasoning_effort_label(Some(
                effort,
            )));
        }
        status_line_from_segments(
            [(StatusLineItem::ModelWithReasoning, label)],
            self.use_theme_colors,
            Some(row.id),
            None,
        )
        .unwrap_or_default()
        .line
    }
}

impl Renderable for RosterView {
    fn desired_height(&self, _width: u16) -> u16 {
        let roster = self
            .roster
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if roster.rows.is_empty() {
            0
        } else {
            u16::try_from(roster.rows.len()).unwrap_or(u16::MAX).min(6) + 1
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let area = area.inset(Insets::tlbr(
            /*top*/ 0, /*left*/ 0, /*bottom*/ 0, /*right*/ 2,
        ));
        if area.is_empty() {
            return;
        }
        let mut roster = self
            .roster
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Preserve at least one selectable row when the terminal is vertically cramped.
        let show_hint = area.height > 1;
        let height = usize::from(area.height) - usize::from(show_hint);
        let hint = match roster.retiring {
            Some(Retirement::Pending) => "  Retiring…",
            Some(Retirement::Reconciling(_)) => "  Checking retirement…",
            None => "  Ctrl+X retire · ↑↓ select · Enter open · Esc back",
        };
        if show_hint {
            Paragraph::new(truncate_line_to_width(
                Line::from(hint).dim(),
                usize::from(area.width),
            ))
            .render(
                Rect {
                    y: area.bottom() - 1,
                    height: 1,
                    ..area
                },
                buf,
            );
        }
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
        let name_width = roster
            .rows
            .iter()
            .map(|row| Line::from(row.name.as_str()).width())
            .max()
            .unwrap_or(0)
            .min(24)
            .min(usize::from(area.width) / 3);
        let model_width = roster
            .rows
            .iter()
            .map(|row| self.model_cell(row).width())
            .max()
            .unwrap_or(0);
        for (offset, row) in
            (0..area.height).zip(roster.rows.iter().skip(roster.first).take(height))
        {
            let tokens = row
                .tokens
                .map_or_else(|| "—".to_string(), format_tokens_compact)
                .dim();
            let tokens_width = tokens.width().min(usize::from(area.width));
            let edge = if offset == 0 && roster.first > 0 {
                '↑'
            } else if usize::from(offset) + 1 == height && roster.first + height < roster.rows.len()
            {
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
            if model_width > 0 {
                let model = self.model_cell(row);
                let padding = model_width.saturating_sub(model.width()) + 1;
                spans.extend(model.spans);
                spans.push(" ".repeat(padding).into());
            }
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
                    y: area.y + offset,
                    height: 1,
                    ..area
                },
                buf,
            );
        }
    }
}
