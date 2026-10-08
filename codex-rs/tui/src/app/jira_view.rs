//! Server-backed issue browser. Drafts live here, never in disposable storage;
//! reload is refused while this surface is open. Reading is never model input.

use super::jira_preview::JiraPreview;
use crate::bottom_pane::TextArea;
use crate::bottom_pane::TextAreaState;
use codex_app_server_protocol::JiraCategory;
use codex_app_server_protocol::JiraIssue;
use codex_app_server_protocol::JiraKey;
use codex_app_server_protocol::JiraRevision;
use codex_protocol::ThreadId;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::Widget;

pub(super) enum JiraIntent {
    List,
    Preview {
        key: JiraKey,
    },
    Read {
        key: JiraKey,
        purpose: ReadPurpose,
        expected: Option<JiraRevision>,
    },
    Save,
    Delete,
}

#[derive(Clone, Copy)]
pub(super) enum ReadPurpose {
    Edit,
    Delete,
    Farm,
    Triage,
}

pub(super) struct JiraBrowser {
    pub(super) thread_id: ThreadId,
    pub(super) project: String,
    pub(super) scope: Option<String>,
    pub(super) keys: Vec<JiraKey>,
    pub(super) selected: Option<JiraKey>,
    pub(super) preview: JiraPreview,
    pub(super) full_body: bool,
    pub(super) form: Option<JiraForm>,
    pub(super) pending: Option<(uuid::Uuid, JiraIntent)>,
    pub(super) error: Option<String>,
    pub(super) filter: TextArea,
    scroll: usize,
    pub(super) reveal_selection: bool,
    targets: JiraTargets,
}

#[derive(Default)]
struct JiraTargets {
    list: Rect,
    body: Rect,
    rows: Vec<(Rect, JiraKey)>,
}

pub(super) struct JiraForm {
    pub(super) previous: Option<JiraIssue>,
    pub(super) category: JiraCategory,
    pub(super) slug: TextArea,
    pub(super) body: TextArea,
    field: usize,
    discard: bool,
    body_state: TextAreaState,
}

impl JiraForm {
    pub(super) fn new(previous: Option<JiraIssue>) -> Self {
        let mut slug = TextArea::new();
        let mut body = TextArea::new();
        let category = previous.as_ref().map_or(JiraCategory::Bug, |issue| {
            issue.revision.key.category.clone()
        });
        if let Some(issue) = &previous {
            slug.set_text_clearing_elements(&issue.revision.key.slug);
            body.set_text_clearing_elements(&issue.body);
        }
        Self {
            previous,
            category,
            slug,
            body,
            field: 1,
            discard: false,
            body_state: TextAreaState::default(),
        }
    }

    pub(super) fn key(&self) -> JiraKey {
        JiraKey {
            project: self
                .previous
                .as_ref()
                .and_then(|issue| issue.revision.key.project.clone()),
            category: self.category.clone(),
            slug: self.slug.text().to_owned(),
        }
    }

    fn dirty(&self) -> bool {
        match &self.previous {
            Some(issue) => self.key() != issue.revision.key || self.body.text() != issue.body,
            None => {
                self.category != JiraCategory::Bug || !self.slug.is_empty() || !self.body.is_empty()
            }
        }
    }

    // Returns true only when the form may be discarded. Save is handled by the owner.
    pub(super) fn input(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Repeat
            && (self.discard
                || self.field == 0
                || matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Tab | KeyCode::BackTab
                ))
        {
            return false;
        }
        if self.discard {
            match key.code {
                KeyCode::Enter => return true,
                KeyCode::Esc => self.discard = false,
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Esc if self.dirty() => self.discard = true,
            KeyCode::Esc => return true,
            KeyCode::Tab => self.field = (self.field + 1) % 3,
            KeyCode::BackTab => self.field = (self.field + 2) % 3,
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if self.field == 0 => {
                self.category = match self.category {
                    JiraCategory::Bug => JiraCategory::Feature,
                    JiraCategory::Feature => JiraCategory::Bug,
                };
            }
            KeyCode::Enter if self.field == 2 => self.body.insert_str("\n"),
            KeyCode::Enter => self.field = (self.field + 1) % 3,
            _ if self.field == 1 => self.slug.input(key),
            _ if self.field == 2 => self.body.input(key),
            _ => {}
        }
        false
    }

    pub(super) fn paste(&mut self, text: &str) {
        match self.field {
            1 if !text.contains(['\n', '\r']) => self.slug.insert_str(text),
            2 => self.body.insert_str(text),
            _ => {}
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) -> Option<(u16, u16)> {
        if area.height < 7 || area.width < 10 {
            Paragraph::new("Enlarge the terminal to edit this issue.").render(area, buf);
            return None;
        }
        let category = format!(
            "{}Category: {}",
            if self.field == 0 { "› " } else { "  " },
            self.category.as_str()
        );
        Paragraph::new(category).render(Rect { height: 1, ..area }, buf);
        let slug_label = format!("{}Slug: ", if self.field == 1 { "› " } else { "  " });
        Paragraph::new(slug_label).render(
            Rect {
                y: area.y + 2,
                height: 1,
                ..area
            },
            buf,
        );
        let slug_area = Rect::new(
            area.x + 8,
            area.y + 2,
            area.width.saturating_sub(/*rhs*/ 8),
            /*height*/ 1,
        );
        ratatui::widgets::WidgetRef::render_ref(&(&self.slug), slug_area, buf);
        Paragraph::new(if self.field == 2 {
            "› Body"
        } else {
            "  Body"
        })
        .render(
            Rect {
                y: area.y + 4,
                height: 1,
                ..area
            },
            buf,
        );
        let body_area = Rect::new(
            area.x + 2,
            area.y + 5,
            area.width.saturating_sub(/*rhs*/ 2),
            area.height.saturating_sub(/*rhs*/ 5),
        );
        StatefulWidgetRef::render_ref(&(&self.body), body_area, buf, &mut self.body_state);
        match self.field {
            1 => self.slug.cursor_pos(slug_area),
            2 => self.body.cursor_pos_with_state(body_area, self.body_state),
            _ => None,
        }
    }
}

impl JiraBrowser {
    pub(super) fn busy(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|(_, intent)| !matches!(intent, JiraIntent::Preview { .. }))
    }

    pub(super) fn new(thread_id: ThreadId) -> Self {
        Self {
            thread_id,
            project: String::new(),
            scope: None,
            keys: vec![],
            selected: None,
            preview: JiraPreview::default(),
            full_body: false,
            form: None,
            pending: None,
            error: None,
            filter: TextArea::new(),
            scroll: 0,
            reveal_selection: true,
            targets: JiraTargets::default(),
        }
    }

    pub(super) fn filtered(&self) -> Vec<JiraKey> {
        let filter = self.filter.text().to_lowercase();
        self.keys
            .iter()
            .filter(|key| self.issue_label(key).to_lowercase().contains(&filter))
            .cloned()
            .collect()
    }

    fn issue_label(&self, key: &JiraKey) -> String {
        let issue = format!("{}/{}", key.category.as_str(), key.slug);
        match &key.project {
            Some(project) => {
                let path = std::path::Path::new(project.as_str());
                let label = path
                    .strip_prefix(&self.project)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                format!("{} · {issue}", if label.is_empty() { "." } else { &label })
            }
            None => issue,
        }
    }

    pub(super) fn select_neighbor(&mut self, delta: isize) {
        let keys = self.filtered();
        if keys.is_empty() {
            self.select(None);
            return;
        }
        let old = keys
            .iter()
            .position(|key| Some(key) == self.selected.as_ref())
            .unwrap_or(/*default*/ 0);
        let index = old.saturating_add_signed(delta).min(keys.len() - 1);
        self.select(Some(keys[index].clone()));
        self.reveal_selection = true;
    }

    pub(super) fn select(&mut self, key: Option<JiraKey>) {
        if self.selected != key {
            self.selected = key;
            self.preview = JiraPreview::default();
            self.reveal_selection = true;
        }
    }

    pub(super) fn filter_key(&mut self, key: KeyEvent) {
        if matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Left
                | KeyCode::Right
        ) {
            self.filter.input(key);
            let keys = self.filtered();
            if !keys.iter().any(|key| Some(key) == self.selected.as_ref()) {
                self.select(keys.first().cloned());
            }
            self.scroll = 0;
            self.reveal_selection = true;
        }
    }

    pub(super) fn scroll_body(&mut self, delta: isize) {
        if self.targets.body.is_empty() {
            self.scroll = self.scroll.saturating_add_signed(delta);
        } else {
            self.preview.scroll(delta);
        }
    }

    pub(super) fn mouse_input(&mut self, mouse: MouseEvent) -> bool {
        let point = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, key)) = self
                    .targets
                    .rows
                    .iter()
                    .find(|(area, _)| area.contains(point))
                    && self.selected.as_ref() != Some(key)
                    && self.filtered().contains(key)
                {
                    self.select(Some(key.clone()));
                    return true;
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta = if mouse.kind == MouseEventKind::ScrollUp {
                    -3
                } else {
                    3
                };
                if self.targets.body.contains(point) {
                    self.preview.scroll(delta);
                    return true;
                }
                if self.targets.list.contains(point) {
                    self.scroll = self.scroll.saturating_add_signed(delta);
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    pub(super) fn render(&mut self, area: Rect, buf: &mut Buffer) -> Option<(u16, u16)> {
        self.targets = JiraTargets::default();
        Clear.render(area, buf);
        if area.height < 10 || area.width < 20 {
            Paragraph::new("Enlarge the terminal to browse issues.").render(area, buf);
            return None;
        }
        let inner = Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(/*rhs*/ 2),
            area.height.saturating_sub(/*rhs*/ 2),
        );
        let title = if self.form.is_some() {
            "Jira · issue"
        } else {
            "Jira"
        };
        let project = self
            .form
            .as_ref()
            .and_then(|form| form.previous.as_ref())
            .and_then(|issue| issue.revision.key.project.as_ref())
            .map_or(self.project.as_str(), |project| project.as_str());
        Paragraph::new(vec![Line::from(title.bold()), Line::from(project).dim()])
            .render(Rect { height: 2, ..inner }, buf);
        let status = if self.form.as_ref().is_some_and(|form| form.discard) {
            "Discard changes? Enter discard · Esc keep editing".to_owned()
        } else if let Some(error) = &self.error {
            error.clone()
        } else if let Some((_, intent)) = &self.pending {
            match intent {
                JiraIntent::Save => "Saving…",
                JiraIntent::Delete => "Deleting…",
                JiraIntent::List => "Loading issues…",
                JiraIntent::Read { .. } => "Reading issue…",
                JiraIntent::Preview { .. } => "",
            }
            .to_owned()
        } else {
            String::new()
        };
        let hints = if self.form.is_some() {
            "Tab field · Ctrl+S save · Esc back"
        } else if self.full_body {
            "↑↓ select · Enter list · PgUp/PgDn scroll · Alt+F farm · Alt+T triage · Ctrl+N new · Ctrl+E edit · Del delete · Esc back"
        } else {
            "↑↓/hover select · Enter body · PgUp/PgDn scroll · Alt+F farm · Alt+T triage · Ctrl+N new · Ctrl+E edit · Del delete · Esc close"
        };
        let hint_lines: Vec<Line<'static>> = textwrap::wrap(hints, inner.width as usize)
            .into_iter()
            .map(|line| Line::from(line.into_owned()).dim())
            .collect();
        let hint_height = (hint_lines.len() as u16).min(inner.height.saturating_sub(/*rhs*/ 3));
        let footer_height = hint_height + 2;
        let footer_y = inner.y + inner.height.saturating_sub(footer_height);
        let status_lines = textwrap::wrap(&status, inner.width as usize)
            .into_iter()
            .map(|line| Line::from(line.into_owned()))
            .collect::<Vec<_>>();
        Paragraph::new(status_lines)
            .render(Rect::new(inner.x, footer_y, inner.width, /*height*/ 2), buf);
        Paragraph::new(hint_lines).render(
            Rect::new(inner.x, footer_y + 2, inner.width, hint_height),
            buf,
        );
        let content = Rect::new(
            inner.x,
            inner.y + 3,
            inner.width,
            inner.height.saturating_sub(3 + footer_height),
        );
        if let Some(form) = &mut self.form {
            return form.render(content, buf);
        }
        Paragraph::new(format!("Filter: {}", self.filter.text())).render(
            Rect {
                height: 1,
                ..content
            },
            buf,
        );
        let mut list = Rect::new(
            content.x,
            content.y + 2,
            content.width,
            content.height.saturating_sub(/*rhs*/ 2),
        );
        if self.full_body {
            self.targets.body = list;
            list = Rect::default();
        } else if list.width >= 80 {
            let width = (list.width / 5 * 2).min(48);
            self.targets.body = Rect::new(
                list.x + width + 3,
                list.y,
                list.width - width - 3,
                list.height,
            );
            list.width = width;
            for y in list.y..list.bottom() {
                Paragraph::new("│".dim()).render(
                    Rect::new(list.right() + 1, y, /*width*/ 1, /*height*/ 1),
                    buf,
                );
            }
        }
        self.targets.list = list;
        if !self.targets.body.is_empty()
            && let Some(key) = &self.selected
        {
            self.preview
                .render(&self.issue_label(key), self.targets.body, buf);
        }
        let keys = self.filtered();
        if keys.is_empty()
            && self.scope.is_some()
            && !matches!(self.pending, Some((_, JiraIntent::List)))
            && self.error.is_none()
        {
            Paragraph::new(if self.keys.is_empty() {
                "No issues. Ctrl+N creates one."
            } else {
                "No matching issues."
            })
            .render(list, buf);
        }
        if self.reveal_selection
            && !list.is_empty()
            && let Some(selected) = keys
                .iter()
                .position(|key| Some(key) == self.selected.as_ref())
        {
            if selected < self.scroll {
                self.scroll = selected;
            } else if selected >= self.scroll + list.height as usize {
                self.scroll =
                    selected.saturating_sub(list.height.saturating_sub(/*rhs*/ 1) as usize);
            }
            self.reveal_selection = false;
        }
        self.scroll = self
            .scroll
            .min(keys.len().saturating_sub(list.height as usize));
        for (row, key) in keys
            .iter()
            .skip(self.scroll)
            .take(list.height as usize)
            .enumerate()
        {
            let selected = self.selected.as_ref() == Some(key);
            let line = format!(
                "{} {}",
                if selected { "›" } else { " " },
                self.issue_label(key)
            );
            let line = if selected {
                Line::from(line.bold())
            } else {
                line.into()
            };
            let row = Rect::new(list.x, list.y + row as u16, list.width, /*height*/ 1);
            Paragraph::new(line).render(row, buf);
            self.targets.rows.push((row, key.clone()));
        }
        None
    }
}

pub(super) fn is_action_key(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers == KeyModifiers::CONTROL
        && key.code == KeyCode::Char('s')
}

#[cfg(test)]
#[path = "jira_view_tests.rs"]
mod tests;
