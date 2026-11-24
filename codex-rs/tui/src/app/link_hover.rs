//! Match pointer and foreground feedback to the last painted click targets.
//! Recheck on repaint so scrolling and live output cannot leave hover on stale content.
//! Keep passive link drags with their original view.

use codex_protocol::ThreadId;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton::Left;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind::Drag;
use crossterm::event::MouseEventKind::Moved;
use crossterm::event::MouseEventKind::ScrollDown;
use crossterm::event::MouseEventKind::ScrollUp;
use crossterm::event::MouseEventKind::Up;
use ratatui::buffer::Buffer;
use ratatui::buffer::CellDiffOption;
use ratatui::buffer::CellWidth;
use ratatui::layout::Position;
use ratatui::layout::Rect;

use super::App;
use crate::app_server_session::AppServerSession;
use crate::custom_terminal::osc8_hyperlink_parts;
use crate::pager_overlay::Overlay;
use crate::transcript_view::TranscriptView;
use crate::transcript_view::ViewAction;
use crate::tui::PointerShape;
use crate::tui::Tui;
use crate::tui::TuiEvent;

impl App {
    pub(super) fn observe_pointer(&self, tui: &mut Tui, event: &TuiEvent) {
        let previous = tui.link_hover.mouse;
        let previous_position = tui.link_hover.position();
        tui.link_hover.observe(event);
        let current = tui.link_hover.mouse;
        if previous == current {
            return;
        }
        let buffer = tui.terminal.previous_buffer();
        if self.hovered_sidebar_target(previous_position)
            != self.hovered_sidebar_target(tui.link_hover.position())
            || self.hovered_sidebar_chat(previous_position)
                != self.hovered_sidebar_chat(tui.link_hover.position())
            || self.overlay_thread_link_hover_area(previous)
                != self.overlay_thread_link_hover_area(current)
            || self.hover_target_area(self.hover_pointer(tui, previous), buffer)
                != self.hover_target_area(self.hover_pointer(tui, current), buffer)
        {
            tui.frame_requester().schedule_frame();
        }
    }

    pub(super) fn hover_pointer(&self, tui: &Tui, mouse: Option<MouseEvent>) -> Option<MouseEvent> {
        mouse.filter(|mouse| {
            tui.is_owned_screen()
                && self.overlay.is_none()
                && !tui.shortcut_hud_contains(*mouse)
                && self.divider_drag.is_none()
                && self.sidebar().is_none_or(|pane| pane.close.is_none())
        })
    }

    pub(super) fn hover_target_area(
        &self,
        mouse: Option<MouseEvent>,
        buf: &Buffer,
    ) -> Option<Rect> {
        let mouse = mouse?;
        let point = Position::new(mouse.column, mouse.row);
        if !buf.area.contains(point) {
            return None;
        }
        let passive = self
            .conversation_frame
            .side_pane()
            .is_some_and(|panes| panes.passive.contains(point));
        let ordinary_input = self.chat_widget.no_modal_or_popup_active();
        if !passive {
            if mouse.modifiers.is_empty()
                && (ordinary_input
                    || (self.chat_widget.has_active_modal()
                        && self.chat_widget.centered_dialog().is_none()))
                && let Some(area) = self.transcript_view.disclosure_hover_area(point)
            {
                return Some(area);
            }
            if let Some(area) = self.chat_widget.warning_notice_at(point) {
                return Some(area);
            }
        }
        if !ordinary_input {
            return None;
        }
        let link_view = self.transcript_link_view(mouse);
        if let Some(area) = link_view.and_then(|view| view.thread_link_hover_area(point)) {
            return Some(area);
        }
        if self.chat_widget.status_line_area().contains(point)
            || link_view.is_some_and(|view| view.link_at(mouse.column, mouse.row).is_some())
        {
            painted_link_area(buf, point)
        } else {
            None
        }
    }

    fn overlay_thread_link_hover_area(&self, mouse: Option<MouseEvent>) -> Option<Rect> {
        let Some(Overlay::Transcript(overlay)) = self.overlay.as_ref() else {
            return None;
        };
        overlay.thread_link_hover_area(mouse?)
    }

    pub(super) fn transcript_link_at(&self, mouse: MouseEvent) -> Option<String> {
        self.transcript_link_view(mouse)?
            .link_at(mouse.column, mouse.row)
    }

    fn transcript_link_view(&self, mouse: MouseEvent) -> Option<&TranscriptView> {
        if !mouse.modifiers.is_empty()
            && !mouse
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        {
            return None;
        }
        let view = if let Some(panes) = self.conversation_frame.side_pane()
            && panes.passive.contains((mouse.column, mouse.row).into())
        {
            &self.side_companions.get(&panes.passive_thread)?.view
        } else {
            &self.transcript_view
        };
        (!view.is_select_mode()).then_some(view)
    }

    pub(super) fn handle_passive_transcript_event(
        &mut self,
        tui: &mut Tui,
        app_server: &AppServerSession,
        id: ThreadId,
        event: &TuiEvent,
    ) -> bool {
        let Some(companion) = self.side_companions.get_mut(&id) else {
            return false;
        };
        let view = &mut companion.view;
        view.mouse_scroll_speed = self.transcript_view.mouse_scroll_speed;
        view.copy_on_select = self.transcript_view.copy_on_select;
        view.copy_on_select_clear_selection = self.transcript_view.copy_on_select_clear_selection;
        view.primary_selection = self.transcript_view.primary_selection;
        let action = match event {
            TuiEvent::Mouse(mouse) => view.handle_mouse(*mouse, &companion.cells),
            TuiEvent::Key(key) if crate::text_selection::is_copy_key(*key) => view
                .handle_selection_key(*key, &companion.cells)
                .filter(|action| matches!(action, ViewAction::Copy(_))),
            _ => None,
        };
        let Some(action) = action else {
            return false;
        };
        if let Some((count, result)) = view.copy_action(
            tui,
            &companion.cells,
            &action,
            crate::clipboard_copy::CopyFormat::Markdown,
        ) {
            self.transcript_view.show_copy_feedback(&result, count);
        } else if let ViewAction::OpenThreadLink(thread_id) = action {
            self.request_thread_link(app_server, Some(id), thread_id);
        } else if let ViewAction::OpenLink(url) = action {
            self.open_url_in_browser(url);
        }
        tui.frame_requester().schedule_frame();
        true
    }

    pub(super) fn continue_passive_link_drag(
        &mut self,
        tui: &mut Tui,
        app_server: &AppServerSession,
        event: &TuiEvent,
    ) -> color_eyre::eyre::Result<bool> {
        let Some((id, area)) = self.passive_link_drag else {
            return Ok(false);
        };
        if self.conversation_focus_available(tui) {
            match event {
                TuiEvent::Draw => {
                    if let Some(companion) = self.side_companions.get_mut(&id)
                        && companion.view.tick_selection(&companion.cells)
                    {
                        tui.frame_requester()
                            .schedule_frame_in(crate::tui::TARGET_FRAME_INTERVAL);
                    }
                    return Ok(false);
                }
                TuiEvent::Mouse(mouse) if mouse.kind == Moved => return Ok(false),
                TuiEvent::Mouse(mouse)
                    if matches!(mouse.kind, Drag(Left) | Up(Left) | ScrollUp | ScrollDown) =>
                {
                    self.refresh_owned_transcript(tui)?;
                    if self
                        .conversation_frame
                        .side_pane()
                        .is_some_and(|panes| panes.passive_thread == id && panes.passive == area)
                    {
                        self.handle_passive_transcript_event(tui, app_server, id, event);
                        if mouse.kind == Up(Left) {
                            self.passive_link_drag = None;
                        }
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
        self.passive_link_drag = None;
        if let Some(companion) = self.side_companions.get_mut(&id) {
            companion.view.end_drag();
        } else if self.current_displayed_thread_id() == Some(id) {
            self.transcript_view.end_drag();
        }
        Ok(false)
    }

    pub(super) fn refresh_link_hover(&self, tui: &mut Tui) -> std::io::Result<()> {
        let shape = if tui
            .link_hover
            .mouse
            .is_some_and(|mouse| tui.shortcut_hud_contains(mouse))
        {
            PointerShape::Default
        } else if self
            .overlay_thread_link_hover_area(tui.link_hover.mouse)
            .is_some()
        {
            PointerShape::Link
        } else if !tui.is_owned_screen() || self.overlay.is_some() {
            PointerShape::Default
        } else if self.conversation_focus_available(tui)
            && (self.divider_drag.is_some()
                || tui.link_hover.mouse.is_some_and(|mouse| {
                    self.conversation_frame
                        .divider_area()
                        .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
                        && self.divider_is_settled(tui, tui.terminal.last_known_screen_size.into())
                }))
        {
            PointerShape::Resize
        } else if self.chat_widget.no_modal_or_popup_active()
            && tui
                .link_hover
                .mouse
                .is_some_and(|mouse| self.transcript_link_at(mouse).is_some())
        {
            PointerShape::Link
        } else {
            PointerShape::Default
        };
        if (shape == PointerShape::Resize) != (tui.link_hover.shape == PointerShape::Resize) {
            tui.frame_requester().schedule_frame();
        }
        tui.link_hover.shape = shape;
        tui.set_pointer_shape(shape)
    }
}

/// Read a single visible link fragment from the OSC 8 markers attached by the renderer.
fn painted_link_area(buf: &Buffer, point: Position) -> Option<Rect> {
    let mut column = buf.area.left();
    let mut start = column;
    let mut destination = None;
    let right = buf.area.right();
    while column < right {
        let cell = &buf[(column, point.y)];
        let link = if cell.diff_option == CellDiffOption::Skip {
            destination
        } else {
            osc8_hyperlink_parts(cell.symbol()).map(|(url, _)| url)
        };
        if link != destination {
            if column > point.x {
                break;
            }
            start = column;
            destination = link;
        }
        // A wide glyph carries the link on its first column only.
        column = column.saturating_add(cell.cell_width().max(1)).min(right);
    }
    destination.map(|_| Rect::new(start, point.y, column - start, /*height*/ 1))
}
