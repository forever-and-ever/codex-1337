//! Hit targets belong to the last painted frame, not to a newly sorted model.
use super::*;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::layout::Position;

#[derive(Debug, Default)]
pub(super) struct Pointer {
    area: Rect,
    visible: Vec<ThreadId>,
    position: Option<Position>,
    pressed: Option<Press>,
    painted: bool,
}

#[derive(Debug, Clone, Copy)]
enum Press {
    Row(ThreadId),
    Thumb(usize),
}

impl Pointer {
    pub(super) fn engaged(&self) -> bool {
        self.pressed.is_some() || self.position.is_some_and(|p| self.area.contains(p))
    }
}

pub(crate) enum MouseAction {
    None,
    Redraw,
    Open(ThreadId),
}

impl SubagentRoster {
    pub(crate) fn begin_frame(&mut self) {
        self.pointer.painted = false;
    }

    pub(crate) fn finish_frame(&mut self) -> bool {
        if !self.pointer.painted {
            self.invalidate_pointer();
        }
        !self.pointer.area.is_empty()
    }

    pub(crate) fn invalidate_pointer(&mut self) {
        let engaged = self.pointer.engaged();
        self.pointer = Pointer::default();
        if engaged || self.deferred.is_some() {
            self.release_order();
        }
    }

    fn release_order(&mut self) {
        if let Some(agents) = self.deferred.take() {
            self.reconcile(agents, self.watched);
        } else {
            self.sort_by_activity();
        }
    }

    pub(super) fn record_view(&mut self, area: Rect) {
        if area != self.pointer.area {
            self.pointer.pressed = None;
        }
        self.pointer.area = area;
        self.pointer.painted = true;
        self.pointer.visible = self
            .rows
            .iter()
            .skip(self.first)
            .take(usize::from(area.height))
            .map(|row| row.id)
            .collect();
    }

    pub(super) fn hovered(&self) -> Option<ThreadId> {
        let position = self.pointer.position?;
        let area = self.pointer.area;
        if !area.contains(position)
            || (self.scrollbar().is_some() && position.x + 1 == area.right())
        {
            return None;
        }
        self.pointer
            .visible
            .get(usize::from(position.y - area.y))
            .copied()
    }

    pub(super) fn scrollbar(&self) -> Option<(usize, usize)> {
        let height = usize::from(self.pointer.area.height);
        let maximum = self.rows.len().saturating_sub(height);
        if height == 0 || maximum == 0 {
            return None;
        }
        // Overflow needs at least one travel cell, even with only one hidden row.
        let length = (height * height)
            .div_ceil(self.rows.len())
            .clamp(1, height.saturating_sub(1).max(1));
        Some((self.first * (height - length) / maximum, length))
    }

    pub(crate) fn mouse(&mut self, event: MouseEvent) -> MouseAction {
        // A wheel/drag changed the viewport but has not been painted yet. Do not
        // interpret a fresh press against the new model and the old visible thumb.
        if !self.pointer.painted && matches!(event.kind, MouseEventKind::Down(_)) {
            return MouseAction::None;
        }
        let hovered = self.hovered();
        let engaged = self.pointer.engaged();
        let old_first = self.first;
        let position = Position::new(event.column, event.row);
        self.pointer.position = Some(position);
        let inside = self.pointer.area.contains(position);
        let height = usize::from(self.pointer.area.height);
        let maximum = self.rows.len().saturating_sub(height);
        if !event.modifiers.is_empty() {
            self.pointer.pressed = None;
        } else {
            match event.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if inside => {
                    self.first = if event.kind == MouseEventKind::ScrollUp {
                        self.first.saturating_sub(1)
                    } else {
                        (self.first + 1).min(maximum)
                    };
                    self.focused = None;
                    self.reveal = None;
                    self.pointer.pressed = None;
                }
                MouseEventKind::Down(MouseButton::Left) if inside => {
                    if let Some((top, length)) = self.scrollbar()
                        && position.x + 1 == self.pointer.area.right()
                    {
                        let y = usize::from(position.y - self.pointer.area.y);
                        if (top..top + length).contains(&y) {
                            self.pointer.pressed = Some(Press::Thumb(y - top));
                        } else {
                            self.first = if y < top {
                                self.first.saturating_sub(height)
                            } else {
                                (self.first + height).min(maximum)
                            };
                        }
                        self.reveal = None;
                        self.focused = None;
                    } else {
                        self.pointer.pressed = self.hovered().map(Press::Row);
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(Press::Thumb(grab)) = self.pointer.pressed
                        && let Some((_, length)) = self.scrollbar()
                    {
                        let travel = height - length;
                        let y = usize::from(position.y.saturating_sub(self.pointer.area.y))
                            .saturating_sub(grab)
                            .min(travel);
                        self.first = (y * maximum + travel / 2).checked_div(travel).unwrap_or(0);
                    } else if self.hovered() != hovered {
                        self.pointer.pressed = None;
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if let Some(Press::Row(id)) = self.pointer.pressed.take()
                        && self.hovered() == Some(id)
                        && self.rows.iter().any(|row| {
                            row.id == id && !matches!(row.status, Some(ThreadStatus::NotLoaded))
                        })
                    {
                        self.focused = None;
                        return MouseAction::Open(id);
                    }
                }
                _ => {}
            }
        }
        if engaged && !self.pointer.engaged() {
            self.release_order();
        }
        if old_first != self.first {
            self.pointer.painted = false;
        }
        if hovered != self.hovered() || old_first != self.first || engaged != self.pointer.engaged()
        {
            MouseAction::Redraw
        } else {
            MouseAction::None
        }
    }
}
