//! Human-only work below the transcript. Disclosures own their height; only terminal bounds clip.

use std::collections::HashSet;

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
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;

use crate::style::accent_style;
use crate::style::secondary_text_style;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Group {
    Work,
    Processes,
    Alarms,
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Self::Work => "Work",
            Self::Processes => "Processes",
            Self::Alarms => "Alarms",
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Key {
    Group(Group),
    Phase,
    ProcessStatus,
    Operation { thread_id: ThreadId, id: String },
    Process { thread_id: ThreadId, id: String },
    Alarm { thread_id: ThreadId, id: String },
    Requests,
    Queue,
    Startup,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Action {
    RevealCall {
        thread_id: ThreadId,
        item_id: String,
    },
    DisarmAlarm {
        thread_id: ThreadId,
        id: String,
    },
    OpenQuestions,
    CleanProcesses {
        thread_id: ThreadId,
    },
}

impl Action {
    fn label(&self) -> &'static str {
        match self {
            Self::RevealCall { .. } => "Show in history",
            Self::DisarmAlarm { .. } => "Disarm alarm",
            Self::OpenQuestions => "Answer questions",
            Self::CleanProcesses { .. } => "Stop all for this owner",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub(crate) key: Key,
    pub(crate) summary: Line<'static>,
    pub(crate) details: Vec<Line<'static>>,
    pub(crate) children: Vec<Row>,
    pub(crate) actions: Vec<Action>,
}

impl Row {
    fn has_disclosure(&self) -> bool {
        !self.details.is_empty() || !self.children.is_empty() || !self.actions.is_empty()
    }

    fn is_visible(&self) -> bool {
        !matches!(self.key, Key::Group(_)) || self.has_disclosure()
    }

    fn find(&self, key: &Key) -> Option<&Self> {
        if &self.key == key {
            Some(self)
        } else {
            self.children.iter().find_map(|row| row.find(key))
        }
    }

    fn parent(&self, key: &Key) -> Option<&Key> {
        if self.children.iter().any(|row| &row.key == key) {
            Some(&self.key)
        } else {
            self.children.iter().find_map(|row| row.parent(key))
        }
    }

    fn collect_keys(&self, keys: &mut HashSet<Key>) {
        assert!(
            keys.insert(self.key.clone()),
            "duplicate live-work key: {:?}",
            self.key
        );
        for child in &self.children {
            child.collect_keys(keys);
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Interaction {
    Ignored,
    Consumed,
    Action(Action),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Focus {
    Row(Key),
    Action { key: Key, action: Action },
}

impl Focus {
    fn key(&self) -> &Key {
        match self {
            Self::Row(key) | Self::Action { key, .. } => key,
        }
    }
}

struct WorkLine {
    line: Line<'static>,
    focus: Option<Focus>,
    group_header: bool,
}

#[derive(Default)]
pub(crate) struct LiveWorkView {
    rows: Vec<Row>,
    open: HashSet<Key>,
    selected: Option<Focus>,
    focused: bool,
    hits: Vec<(Rect, Focus)>,
}

impl LiveWorkView {
    pub(crate) fn set_rows(&mut self, mut rows: Vec<Row>) {
        rows.retain(Row::is_visible);
        let mut keys = HashSet::new();
        for row in &rows {
            row.collect_keys(&mut keys);
        }
        let work = Key::Group(Group::Work);
        if self.find(&work).is_none() && keys.contains(&work) {
            self.open.insert(work);
        }
        self.open.retain(|key| keys.contains(key));
        let mut selected = self.selected.take();
        while let Some(focus) = &selected {
            if keys.contains(focus.key()) {
                break;
            }
            selected = self.parent(focus.key()).cloned().map(Focus::Row);
        }
        self.rows = rows;
        self.selected = selected.map(|focus| match focus {
            Focus::Action {
                ref key,
                ref action,
            } if !self
                .find(key)
                .is_some_and(|row| row.actions.contains(action)) =>
            {
                Focus::Row(key.clone())
            }
            focus => focus,
        });
        self.hits.clear();
        if self.focused && self.selected.is_none() {
            self.selected = self.controls().into_iter().next();
        }
        if self.selected.is_none() {
            self.focused = false;
        }
    }

    pub(crate) fn focus_group(&mut self, group: Group) {
        let key = Key::Group(group);
        if self.find(&key).is_some_and(Row::is_visible) {
            self.open.insert(key.clone());
            self.selected = Some(Focus::Row(key));
            self.focused = true;
            self.hits.clear();
        }
    }

    pub(crate) fn blur(&mut self) {
        self.focused = false;
    }

    pub(crate) fn is_focused(&self) -> bool {
        self.focused
    }

    pub(crate) fn desired_height(&self, width: u16) -> u16 {
        u16::try_from(self.lines(width).len()).unwrap_or(u16::MAX)
    }

    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        self.hits.clear();
        Clear.render(area, buf);
        if area.is_empty() {
            return;
        }
        let lines = self.lines(area.width);
        let height = usize::from(area.height);
        let clipped = lines.len() > height;
        let mut budget = height.saturating_sub(usize::from(clipped));
        let mut remaining_headers = lines.iter().filter(|line| line.group_header).count();
        let mut y = area.y;
        let mut shown = 0;
        // Reserve the other groups' folds before spending terminal space on expanded details.
        for work_line in &lines {
            if work_line.group_header {
                remaining_headers = remaining_headers.saturating_sub(1);
            } else if clipped && budget <= remaining_headers {
                continue;
            }
            if budget == 0 {
                continue;
            }
            let line_area = Rect::new(area.x, y, area.width, 1);
            let mut line = work_line.line.clone();
            if self.focused && work_line.focus.as_ref() == self.selected.as_ref() {
                line = line.patch_style(accent_style().underlined());
            }
            line.render(line_area, buf);
            if let Some(focus) = &work_line.focus {
                self.hits.push((line_area, focus.clone()));
            }
            y = y.saturating_add(1);
            shown += 1;
            budget -= 1;
        }
        if clipped {
            Line::styled(
                format!("… {} lines hidden · ← fold", lines.len() - shown),
                secondary_text_style(),
            )
            .render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }

    pub(crate) fn handle_mouse(&mut self, event: MouseEvent) -> Interaction {
        if event.kind != MouseEventKind::Down(MouseButton::Left) {
            return Interaction::Ignored;
        }
        let Some((_, focus)) = self
            .hits
            .iter()
            .find(|(area, _)| area.contains(Position::new(event.column, event.row)))
        else {
            return Interaction::Ignored;
        };
        self.selected = Some(focus.clone());
        self.focused = true;
        self.activate()
    }

    pub(crate) fn handle_key(&mut self, event: KeyEvent) -> Interaction {
        if !self.focused
            || event.kind == KeyEventKind::Release
            || event
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Interaction::Ignored;
        }
        match event.code {
            KeyCode::Esc => self.blur(),
            KeyCode::Up | KeyCode::BackTab => self.move_focus(false),
            KeyCode::Down | KeyCode::Tab => self.move_focus(true),
            KeyCode::Home => self.selected = self.controls().into_iter().next(),
            KeyCode::End => self.selected = self.controls().into_iter().next_back(),
            KeyCode::Left => self.fold(),
            KeyCode::Right => {
                if let Some(key) = self.selected.as_ref().map(Focus::key)
                    && self.find(key).is_some_and(Row::has_disclosure)
                {
                    self.open.insert(key.clone());
                    self.hits.clear();
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') if event.kind == KeyEventKind::Press => {
                return self.activate();
            }
            KeyCode::Enter | KeyCode::Char(' ') => {}
            _ => return Interaction::Ignored,
        }
        Interaction::Consumed
    }

    fn find(&self, key: &Key) -> Option<&Row> {
        self.rows.iter().find_map(|row| row.find(key))
    }

    fn parent(&self, key: &Key) -> Option<&Key> {
        self.rows.iter().find_map(|row| row.parent(key))
    }

    fn activate(&mut self) -> Interaction {
        match self.selected.clone() {
            Some(Focus::Action { action, .. }) => return Interaction::Action(action),
            Some(Focus::Row(key)) if self.find(&key).is_some_and(Row::has_disclosure) => {
                if !self.open.remove(&key) {
                    self.open.insert(key);
                }
                self.hits.clear();
            }
            _ => {}
        }
        Interaction::Consumed
    }

    fn fold(&mut self) {
        let Some(focus) = self.selected.clone() else {
            return;
        };
        match focus {
            Focus::Action { key, .. } => self.selected = Some(Focus::Row(key)),
            Focus::Row(key) if self.open.remove(&key) => {}
            Focus::Row(key) => {
                if let Some(parent) = self.parent(&key) {
                    self.selected = Some(Focus::Row(parent.clone()));
                }
            }
        }
        self.hits.clear();
    }

    fn move_focus(&mut self, forward: bool) {
        let controls = self.controls();
        if controls.is_empty() {
            return;
        }
        let index = self
            .selected
            .as_ref()
            .and_then(|selected| controls.iter().position(|control| control == selected));
        let next = match (index, forward) {
            (Some(index), true) => (index + 1).min(controls.len() - 1),
            (Some(index), false) => index.saturating_sub(1),
            (None, true) => 0,
            (None, false) => controls.len() - 1,
        };
        self.selected = Some(controls[next].clone());
    }

    fn controls(&self) -> Vec<Focus> {
        let mut controls = Vec::new();
        for (_, focus) in &self.hits {
            if controls.last() != Some(focus) {
                controls.push(focus.clone());
            }
        }
        if controls.is_empty() {
            for line in self.lines(u16::MAX) {
                if let Some(focus) = line.focus
                    && controls.last() != Some(&focus)
                {
                    controls.push(focus);
                }
            }
        }
        controls
    }

    fn lines(&self, width: u16) -> Vec<WorkLine> {
        let mut lines = Vec::new();
        if width != 0 {
            for row in &self.rows {
                self.append_row(row, 0, width, &mut lines);
            }
        }
        lines
    }

    fn append_row(&self, row: &Row, depth: usize, width: u16, lines: &mut Vec<WorkLine>) {
        if !row.is_visible() {
            return;
        }
        let expanded = self.open.contains(&row.key);
        let disclosure = match (row.has_disclosure(), expanded) {
            (true, true) => "▾ ",
            (true, false) => "▸ ",
            (false, _) => "  ",
        };
        let summary = match row.key {
            Key::Group(group) => {
                let count = row
                    .children
                    .iter()
                    .filter(|child| child.is_visible() && child.key != Key::ProcessStatus)
                    .count();
                let title = if count == 0 {
                    group.title().to_owned()
                } else {
                    format!("{} ({count})", group.title())
                };
                Line::styled(title, accent_style())
            }
            _ => row.summary.clone(),
        };
        let start = lines.len();
        append_lines(
            summary,
            depth * 2,
            disclosure,
            width,
            Some(Focus::Row(row.key.clone())),
            lines,
        );
        if matches!(row.key, Key::Group(_)) {
            lines[start].group_header = true;
        }
        if !expanded {
            return;
        }
        for detail in &row.details {
            append_lines(detail.clone(), (depth + 1) * 2, "  ", width, None, lines);
        }
        for action in &row.actions {
            append_lines(
                Line::styled(action.label(), accent_style().underlined()),
                (depth + 1) * 2,
                "  ",
                width,
                Some(Focus::Action {
                    key: row.key.clone(),
                    action: action.clone(),
                }),
                lines,
            );
        }
        for child in &row.children {
            self.append_row(child, depth + 1, width, lines);
        }
    }
}

fn append_lines(
    mut line: Line<'static>,
    indent: usize,
    disclosure: &'static str,
    width: u16,
    focus: Option<Focus>,
    lines: &mut Vec<WorkLine>,
) {
    // Live rows never inherit transcript/speaker fills, including fills on individual spans.
    line.style = line.style.bg(Color::Reset);
    for span in &mut line.spans {
        span.style = span.style.bg(Color::Reset);
    }
    let indent = indent.min(usize::from(width).saturating_sub(3));
    let prefix = Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(disclosure, secondary_text_style()),
    ]);
    let options = RtOptions::new(usize::from(width))
        .initial_indent(prefix)
        .subsequent_indent(Line::from(" ".repeat(indent + 2)));
    for line in word_wrap_lines([line], options) {
        lines.push(WorkLine {
            line,
            focus: focus.clone(),
            group_header: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: Key, summary: &str, children: Vec<Row>) -> Row {
        Row {
            key,
            summary: Line::from(summary.to_owned()),
            details: Vec::new(),
            children,
            actions: Vec::new(),
        }
    }

    #[test]
    fn reconciliation_keeps_folds_and_action_owners_not_row_positions() {
        // A process update can reorder rows between an action selection and its activation.
        let owner = ThreadId::new();
        let other = ThreadId::new();
        let process = |thread_id| {
            let mut process = row(
                Key::Process {
                    thread_id,
                    id: "session".into(),
                },
                "running",
                Vec::new(),
            );
            process.actions.push(Action::CleanProcesses { thread_id });
            process
        };
        let inventory = |children| vec![row(Key::Group(Group::Processes), "", children)];
        let mut view = LiveWorkView::default();
        view.set_rows(inventory(vec![process(owner), process(other)]));
        view.focus_group(Group::Processes);
        view.handle_key(KeyCode::Down.into());
        view.handle_key(KeyCode::Right.into());
        view.handle_key(KeyCode::Down.into());
        view.set_rows(inventory(vec![process(other), process(owner)]));
        assert_eq!(
            view.handle_key(KeyCode::Enter.into()),
            Interaction::Action(Action::CleanProcesses { thread_id: owner }),
        );
        let height = view.desired_height(60);
        view.set_rows(inventory(vec![process(owner)]));
        assert_eq!(view.desired_height(60), height - 1);
        view.set_rows(inventory(vec![process(other)]));
        assert_eq!(
            view.selected,
            Some(Focus::Row(Key::Group(Group::Processes)))
        );
        view.handle_key(KeyCode::Left.into());
        assert_eq!(view.desired_height(60), 1);
    }

    #[test]
    fn terminal_clipping_keeps_group_folds_and_rejects_stale_mouse_targets() {
        // Expanding Work must not strand the folds that reclaim terminal space.
        let mut phase = row(Key::Phase, "Running", Vec::new());
        phase.details = (0..20).map(|n| Line::from(format!("detail {n}"))).collect();
        let mut view = LiveWorkView::default();
        view.set_rows(vec![
            row(Key::Group(Group::Work), "", vec![phase]),
            row(
                Key::Group(Group::Processes),
                "",
                vec![row(Key::ProcessStatus, "No processes", Vec::new())],
            ),
            row(
                Key::Group(Group::Alarms),
                "",
                vec![row(Key::Startup, "Armed", Vec::new())],
            ),
        ]);
        view.focus_group(Group::Work);
        view.handle_key(KeyCode::Down.into());
        view.handle_key(KeyCode::Right.into());
        assert_eq!(view.desired_height(60), 24);
        let area = Rect::new(2, 3, 60, 5);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        for group in [Group::Work, Group::Processes, Group::Alarms] {
            assert!(
                view.hits
                    .iter()
                    .any(|(_, focus)| focus == &Focus::Row(Key::Group(group)))
            );
        }
        let (hit, _) = view
            .hits
            .iter()
            .find(|(_, focus)| *focus == Focus::Row(Key::Group(Group::Work)))
            .unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(view.handle_mouse(click), Interaction::Consumed);
        assert_eq!(view.desired_height(60), 3);
        assert_eq!(view.handle_mouse(click), Interaction::Ignored);
    }
}
