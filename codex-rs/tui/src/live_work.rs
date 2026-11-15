//! Human-only work below the transcript. Disclosures own their height; only terminal bounds clip.
//! Each paint replaces control hits and the text-selection surface, including an empty paint.

pub(crate) mod operations;
mod presented;

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
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;

use crate::style::accent_style;
use crate::style::secondary_text_style;
use crate::wrapping::RtOptions;

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
    ProcessStatus,
    Operation {
        thread_id: ThreadId,
        id: String,
    },
    CodeMode {
        thread_id: ThreadId,
        exec_call_id: String,
    },
    CodeModeCall {
        thread_id: ThreadId,
        exec_call_id: String,
        call_id: String,
    },
    CodeModeWait {
        thread_id: ThreadId,
        exec_call_id: String,
        call_id: String,
    },
    Process {
        thread_id: ThreadId,
        id: String,
    },
    Alarm {
        thread_id: ThreadId,
        id: String,
    },
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
    TerminateProcess {
        thread_id: ThreadId,
        process_id: String,
    },
}

impl Action {
    fn label(&self) -> &'static str {
        match self {
            Self::RevealCall { .. } => "Show in history",
            Self::DisarmAlarm { .. } => "Disarm alarm",
            Self::TerminateProcess { .. } => "Stop process",
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
    line: crate::terminal_hyperlinks::HyperlinkLine,
    focus: Option<Focus>,
    group_header: bool,
}

struct Hover {
    position: Position,
    pending_rows: Option<Vec<Row>>,
}

#[derive(Default)]
pub(crate) struct LiveWorkView {
    rows: Vec<Row>,
    open: HashSet<Key>,
    selected: Option<Focus>,
    focused: bool,
    hits: Vec<(Rect, Focus)>,
    painted_hits: presented::FrameHits,
    hover: Option<Hover>,
    selection: crate::transcript_view::TranscriptView,
}

impl LiveWorkView {
    pub(crate) fn set_rows(&mut self, mut rows: Vec<Row>) {
        if let Some(hover) = &mut self.hover {
            hover.pending_rows = Some(rows);
            return;
        }
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
        let visible = self.controls();
        while self
            .selected
            .as_ref()
            .is_some_and(|selected| !visible.contains(selected))
        {
            self.selected = match self.selected.take() {
                Some(Focus::Action { key, .. }) => Some(Focus::Row(key)),
                Some(Focus::Row(key)) => self.parent(&key).cloned().map(Focus::Row),
                None => None,
            };
        }
        if self.focused && self.selected.is_none() {
            self.selected = self.controls().into_iter().next();
        }
        if self.selected.is_none() {
            self.focused = false;
        }
    }

    pub(crate) fn focus_group(&mut self, group: Group) {
        self.selection.jump_to_latest();
        self.release_hover();
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
        self.selection.end_drag();
        if !self.selection.has_active_interaction() {
            self.release_hover();
        }
    }

    pub(crate) fn is_focused(&self) -> bool {
        self.focused
    }

    pub(crate) fn desired_height(&self, width: u16) -> u16 {
        u16::try_from(
            self.lines(crate::render::live_section::content_width(width))
                .len(),
        )
        .unwrap_or(u16::MAX)
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "Painted rows are clipped to area.height, a u16, before the selection rectangle is created."
    )]
    pub(crate) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        self.hits.clear();
        Clear.render(area, buf);
        let area = crate::render::live_section::content_area(area);
        let lines = if area.is_empty() {
            Vec::new()
        } else {
            self.lines(area.width)
        };
        let height = usize::from(area.height);
        let clipped = lines.len() > height;
        let mut budget = height.saturating_sub(usize::from(clipped));
        let mut remaining_headers = lines.iter().filter(|line| line.group_header).count();
        let mut y = area.y;
        let mut shown = 0;
        let mut painted = Vec::new();
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
            let available = match &work_line.focus {
                Some(Focus::Action { key, action }) => self.action_available(key, action),
                _ => true,
            };
            let completed = matches!(&work_line.focus, Some(Focus::Row(key)) if !self.current_rows().iter().any(|row| row.find(key).is_some()));
            if completed {
                let disclosure = work_line
                    .focus
                    .as_ref()
                    .map(Focus::key)
                    .filter(|key| self.find(key).is_some_and(Row::has_disclosure))
                    .map_or_else(
                        || " ".repeat(crate::render::disclosure::columns() + 1),
                        |key| {
                            format!(
                                "{} ",
                                crate::render::disclosure::glyph(self.open.contains(key))
                            )
                        },
                    );
                line = Line::styled(
                    format!("{disclosure}No longer active"),
                    secondary_text_style(),
                )
                .into();
                let mut source =
                    crate::terminal_hyperlinks::LogicalLineSource::new("No longer active".into());
                source.prefix_bytes = disclosure.len();
                line.source = Some(source);
            } else if !available {
                line = Line::styled(
                    format!(
                        "{}No longer available",
                        " ".repeat(crate::render::disclosure::columns() + 1)
                    ),
                    secondary_text_style(),
                )
                .into();
                let mut source = crate::terminal_hyperlinks::LogicalLineSource::new(
                    "No longer available".into(),
                );
                source.prefix_bytes = crate::render::disclosure::columns() + 1;
                line.source = Some(source);
            } else if self.focused && work_line.focus.as_ref() == self.selected.as_ref() {
                line.line = line.line.patch_style(match &work_line.focus {
                    Some(Focus::Row(_)) => crate::render::disclosure::style(true),
                    _ => accent_style().underlined(),
                });
            }
            if available && let Some(focus) = &work_line.focus {
                match focus {
                    Focus::Row(key) if self.find(key).is_some_and(Row::has_disclosure) => {
                        if let Some(source) = &line.source
                            && source.range.start == 0
                        {
                            // The prefix is counted in bytes: the indent, the glyph and a space.
                            let indent = source.prefix_bytes.saturating_sub(
                                crate::render::disclosure::glyph(/*expanded*/ false).len() + 1,
                            );
                            if let Ok(indent) = u16::try_from(indent)
                                && let Ok(columns) =
                                    u16::try_from(crate::render::disclosure::columns())
                                && indent.saturating_add(columns) <= line_area.width
                            {
                                self.hits.push((
                                    Rect::new(line_area.x + indent, line_area.y, columns, 1),
                                    focus.clone(),
                                ));
                            }
                        }
                    }
                    Focus::Action { .. } => self.hits.push((line_area, focus.clone())),
                    Focus::Row(_) => {}
                }
            }
            painted.push(line);
            y = y.saturating_add(1);
            shown += 1;
            budget -= 1;
        }
        if clipped {
            let hint = crate::line_truncation::truncate_line_to_width(
                Line::styled(
                    format!("… {} lines hidden · ← fold", lines.len() - shown),
                    secondary_text_style(),
                ),
                usize::from(area.width),
            );
            painted.push(hint.into());
        }
        if painted.is_empty() {
            self.focused = false;
            self.selection.jump_to_latest();
            self.release_hover();
        }
        self.selection.render_surface(
            Rect::new(area.x, area.y, area.width, painted.len() as u16),
            buf,
            painted,
        );
    }

    pub(crate) fn selection_event(
        &mut self,
        event: &crate::tui::TuiEvent,
    ) -> Option<crate::transcript_view::ViewAction> {
        match event {
            crate::tui::TuiEvent::Mouse(mouse) => {
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && !mouse.modifiers.contains(KeyModifiers::SHIFT)
                    && self
                        .hits
                        .iter()
                        .any(|(area, _)| area.contains(Position::new(mouse.column, mouse.row)))
                {
                    self.selection.jump_to_latest();
                    self.release_hover();
                    return None;
                }
                if mouse.kind == MouseEventKind::Moved && self.selection.has_active_interaction() {
                    return Some(crate::transcript_view::ViewAction::Changed);
                }
                let action = self.selection.handle_selection_mouse(*mouse, &[], None);
                if action.is_some() {
                    self.focused = false;
                    if self.selection.has_active_interaction() {
                        self.hover.get_or_insert(Hover {
                            position: Position::new(mouse.column, mouse.row),
                            pending_rows: None,
                        });
                    } else {
                        self.release_hover();
                    }
                } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    self.selection.jump_to_latest();
                    self.release_hover();
                }
                action
            }
            crate::tui::TuiEvent::Key(key) if self.selection.has_active_interaction() => {
                let action = self.selection.handle_key(*key, &[]);
                if !self.selection.has_active_interaction() {
                    self.selection.jump_to_latest();
                    self.release_hover();
                }
                action
            }
            _ => None,
        }
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.selection.has_active_interaction()
    }

    pub(crate) fn owns_selection_key(&self, key: KeyEvent) -> bool {
        self.has_selection() && self.selection.owns_interaction_key(key)
    }

    pub(crate) fn selection_view(&mut self) -> &mut crate::transcript_view::TranscriptView {
        &mut self.selection
    }

    pub(crate) fn handle_mouse(&mut self, event: MouseEvent) -> Interaction {
        let position = Position::new(event.column, event.row);
        if event.kind == MouseEventKind::Moved {
            if self
                .hover
                .as_ref()
                .is_some_and(|hover| hover.position == position)
            {
                return Interaction::Ignored;
            }
            let over_control = self.hits.iter().any(|(area, _)| area.contains(position));
            let updated = self.release_hover();
            if over_control {
                self.hover = Some(Hover {
                    position,
                    pending_rows: None,
                });
            }
            return if updated {
                Interaction::Consumed
            } else {
                Interaction::Ignored
            };
        }
        if event.kind != MouseEventKind::Down(MouseButton::Left) {
            return Interaction::Ignored;
        }
        let Some((_, focus)) = self.hits.iter().find(|(area, _)| area.contains(position)) else {
            self.blur();
            return Interaction::Ignored;
        };
        self.selected = Some(focus.clone());
        self.focused = true;
        if self.hover.is_none() {
            self.hover = Some(Hover {
                position,
                pending_rows: None,
            });
        }
        self.activate()
    }

    pub(crate) fn handle_key(&mut self, event: KeyEvent) -> Interaction {
        self.release_hover();
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

    fn release_hover(&mut self) -> bool {
        let pending = self.hover.take().and_then(|hover| hover.pending_rows);
        if let Some(rows) = pending {
            self.set_rows(rows);
            true
        } else {
            false
        }
    }

    fn current_rows(&self) -> &[Row] {
        self.hover
            .as_ref()
            .and_then(|hover| hover.pending_rows.as_deref())
            .unwrap_or(&self.rows)
    }

    fn action_available(&self, key: &Key, action: &Action) -> bool {
        self.current_rows()
            .iter()
            .find_map(|row| row.find(key))
            .is_some_and(|row| row.actions.contains(action))
    }

    fn activate(&mut self) -> Interaction {
        match self.selected.clone() {
            Some(Focus::Action { key, action }) if self.action_available(&key, &action) => {
                return Interaction::Action(action);
            }
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
        let disclosure = row.has_disclosure().then_some(expanded);
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
                Line::styled(title, crate::render::header::style())
            }
            _ => row.summary.clone(),
        };
        let start = lines.len();
        append_lines(
            summary,
            depth * 2,
            disclosure,
            width,
            Some(&Focus::Row(row.key.clone())),
            lines,
        );
        if matches!(row.key, Key::Group(_)) {
            lines[start].group_header = true;
        }
        if !expanded {
            return;
        }
        for detail in &row.details {
            append_lines(detail.clone(), (depth + 1) * 2, None, width, None, lines);
        }
        for action in &row.actions {
            append_lines(
                Line::styled(action.label(), accent_style().underlined()),
                (depth + 1) * 2,
                None,
                width,
                Some(&Focus::Action {
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
    disclosure: Option<bool>,
    width: u16,
    focus: Option<&Focus>,
    lines: &mut Vec<WorkLine>,
) {
    // Live rows never inherit transcript/speaker fills, including fills on individual spans.
    line.style = line.style.bg(Color::Reset);
    for span in &mut line.spans {
        span.style = span.style.bg(Color::Reset);
    }
    let control_width = crate::render::disclosure::columns() + 1;
    let indent = indent.min(usize::from(width).saturating_sub(control_width + 1));
    let mut prefix = Line::from(" ".repeat(indent));
    if let Some(expanded) = disclosure {
        prefix.push_span(Span::styled(
            crate::render::disclosure::glyph(expanded),
            crate::render::disclosure::style(false),
        ));
        prefix.push_span(" ");
    } else {
        prefix.push_span(" ".repeat(control_width));
    }
    let options = RtOptions::new(usize::from(width))
        .initial_indent(prefix)
        .subsequent_indent(Line::from(" ".repeat(indent + control_width)));
    for line in crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(&[line.into()], options) {
        lines.push(WorkLine {
            line,
            focus: focus.cloned(),
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
            process.actions.push(Action::TerminateProcess {
                thread_id,
                process_id: "session".into(),
            });
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
            Interaction::Action(Action::TerminateProcess {
                thread_id: owner,
                process_id: "session".into()
            }),
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

        // Hover holds geometry, but never keeps a completed process actionable.
        view.set_rows(inventory(vec![process(owner), process(other)]));
        view.focus_group(Group::Processes);
        view.handle_key(KeyCode::Down.into());
        view.handle_key(KeyCode::Right.into());
        view.handle_key(KeyCode::Down.into());
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let (hit, target) = view
            .hits
            .iter()
            .find(|(_, focus)| matches!(focus, Focus::Action { .. }))
            .unwrap()
            .clone();
        let mouse = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        view.handle_mouse(mouse(MouseEventKind::Moved, hit.x, hit.y));
        view.set_rows(inventory(vec![process(other), process(owner)]));
        view.render(area, &mut buf);
        assert!(
            view.hits
                .iter()
                .any(|(area, focus)| *area == hit && *focus == target)
        );
        assert_eq!(
            view.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), hit.x, hit.y)),
            Interaction::Action(Action::TerminateProcess {
                thread_id: owner,
                process_id: "session".into()
            })
        );
        view.set_rows(inventory(vec![process(other)]));
        assert_eq!(
            view.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), hit.x, hit.y)),
            Interaction::Consumed
        );
        view.render(area, &mut buf);
        assert!(!view.hits.iter().any(|(_, focus)| *focus == target));
        let display: String = buf
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(display.contains("No longer active"));
        assert_eq!(
            view.handle_mouse(mouse(MouseEventKind::Moved, area.right(), area.bottom())),
            Interaction::Consumed
        );
        assert!(
            view.find(&Key::Process {
                thread_id: owner,
                id: "session".into()
            })
            .is_none()
        );
        // A lifecycle change may move the same operation under a newly folded parent.
        let mut view = LiveWorkView::default();
        view.set_rows(inventory(vec![process(owner)]));
        view.focus_group(Group::Processes);
        view.handle_key(KeyCode::Down.into());
        view.handle_key(KeyCode::Right.into());
        view.handle_key(KeyCode::Down.into());
        let parent = Key::Operation {
            thread_id: owner,
            id: "container".into(),
        };
        view.set_rows(inventory(vec![row(
            parent.clone(),
            "Running",
            vec![process(owner)],
        )]));
        assert_eq!(view.selected, Some(Focus::Row(parent)));
        assert_eq!(
            view.handle_key(KeyCode::Enter.into()),
            Interaction::Consumed
        );
    }

    #[test]
    fn terminal_clipping_keeps_group_folds_and_rejects_stale_mouse_targets() {
        // Expanding Work must not strand the folds that reclaim terminal space.
        let mut phase = row(
            Key::Operation {
                thread_id: ThreadId::new(),
                id: "call".into(),
            },
            "Running",
            Vec::new(),
        );
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
                vec![row(
                    Key::Alarm {
                        thread_id: ThreadId::new(),
                        id: "alarm".into(),
                    },
                    "Armed",
                    Vec::new(),
                )],
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
            let (hit, _) = view
                .hits
                .iter()
                .find(|(_, focus)| focus == &Focus::Row(Key::Group(group)))
                .unwrap();
            assert_eq!(hit.x, area.x + crate::ui_consts::LIVE_PREFIX_COLS);
            let glyph: String = (hit.x..hit.x + 3)
                .map(|x| buf[(x, hit.y)].symbol())
                .collect();
            assert_eq!(
                glyph,
                crate::render::disclosure::glyph(group == Group::Work)
            );
            let title = &buf[(hit.x + 4, hit.y)];
            assert_eq!(title.fg, crate::render::header::style().fg.unwrap());
            assert!(title.modifier.contains(ratatui::style::Modifier::BOLD));
        }
        let (hit, _) = view
            .hits
            .iter()
            .find(|(_, focus)| Some(focus) == view.selected.as_ref())
            .unwrap();
        let selected = &buf[(hit.x + 2, hit.y)];
        assert_eq!(
            selected.fg,
            crate::render::disclosure::style(true).fg.unwrap()
        );
        assert!(
            selected
                .modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
        assert!(!selected.modifier.contains(ratatui::style::Modifier::BOLD));
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
        // Selection uses logical source, not wrapped rows or the disclosure/action hit targets.
        let command = "Command · printf 'alpha   beta gamma 界界界 delta'";
        let operation = row(
            Key::Operation {
                thread_id: ThreadId::new(),
                id: "copy".into(),
            },
            command,
            Vec::new(),
        );
        view.set_rows(vec![row(Key::Group(Group::Work), "", vec![operation])]);
        view.focus_group(Group::Work);
        let area = Rect::new(2, 3, 30, 10);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let mouse = |kind, column, row| {
            crate::tui::TuiEvent::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        assert!(
            view.selection_event(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + crate::ui_consts::LIVE_PREFIX_COLS,
                area.y + 1
            ))
            .is_some()
        );
        view.selection_event(&mouse(
            MouseEventKind::Drag(MouseButton::Left),
            area.right() - 1,
            area.bottom() - 1,
        ));
        view.selection_event(&mouse(
            MouseEventKind::Up(MouseButton::Left),
            area.right() - 1,
            area.bottom() - 1,
        ));
        assert_eq!(view.selection.selected_text(&[]).as_deref(), Some(command));
        view.set_rows(vec![row(
            Key::Group(Group::Work),
            "",
            vec![row(Key::ProcessStatus, "New work", Vec::new())],
        )]);
        let area = Rect::new(2, 3, 24, 10);
        view.render(area, &mut Buffer::empty(area));
        assert_eq!(view.selection.selected_text(&[]).as_deref(), Some(command));
        // An open bottom-pane view hides this panel while its controls occupy the panel's old rows.
        // The hidden text surface must not intercept their clicks, even with a held selection.
        view.render(Rect::default(), &mut Buffer::empty(Rect::default()));
        view.capture_painted_frame();
        assert_eq!(view.handle_mouse(click), Interaction::Ignored);
        assert!(
            view.selection_event(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + crate::ui_consts::LIVE_PREFIX_COLS,
                area.y + 1,
            ))
            .is_none()
        );
        // A present composer gap can allocate no live rows after work completes or clipping.
        // Its former text surface must yield those rows to the growing transcript.
        for empty in [
            Rect { height: 0, ..area },
            Rect {
                width: crate::ui_consts::LIVE_PREFIX_COLS,
                ..area
            },
        ] {
            view.render(area, &mut Buffer::empty(area));
            view.capture_painted_frame();
            assert!(
                view.selection_event(&mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    area.x + crate::ui_consts::LIVE_PREFIX_COLS,
                    area.y + 1,
                ))
                .is_some()
            );
            view.render(empty, &mut Buffer::empty(empty));
            view.capture_painted_frame();
            assert_eq!(view.handle_mouse(click), Interaction::Ignored);
            assert!(
                view.selection_event(&mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    area.x + crate::ui_consts::LIVE_PREFIX_COLS,
                    area.y + 1,
                ))
                .is_none()
            );
        }
    }
}
