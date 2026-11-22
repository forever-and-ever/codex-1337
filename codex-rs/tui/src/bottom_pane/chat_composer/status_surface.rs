//! Separate persistent transcript context from the composer's transient hint row.
//! Measurement, rendering, and cursor placement share the same reserved status rectangle.
//! Shortcut help grows above the input, preserving status and close rows below it even when clipped.

use super::*;

use crate::bottom_pane::footer::max_left_width_for_right;
use crate::render::ruled_sections::HorizontalRule;
use ratatui::layout::Size;

impl ChatComposer {
    pub(crate) fn set_status_line(&mut self, status_line: Option<StatusLine>) -> bool {
        if self.footer.status_line_value == status_line {
            return false;
        }
        if status_line
            .as_ref()
            .is_some_and(|status| status.as_native().is_none())
        {
            self.effort_status_line_transition = None;
        }
        *self.footer.labeled_status_line.get_mut() = None;
        self.footer.status_line_value = status_line;
        true
    }

    pub(crate) fn set_status_line_enabled(&mut self, enabled: bool) -> bool {
        if self.footer.status_line_enabled == enabled {
            return false;
        }
        self.footer.status_line_enabled = enabled;
        true
    }

    pub(crate) fn set_side_conversation_context_label(&mut self, label: Option<String>) -> bool {
        if self.footer.side_conversation_context_label == label {
            return false;
        }
        self.footer.side_conversation_context_label = label;
        true
    }

    pub(super) fn status_surface_height(
        &self,
        mut bounds: Size,
        options: ComposerRenderOptions<'_>,
    ) -> u16 {
        if !self.footer.status_line_enabled || bounds.height == 0 {
            return 0;
        }
        let focus = options.status_line_focus();
        let rule = u16::from(options.separate_status_line && bounds.height > 1);
        bounds.height -= rule;
        // Fullscreen native context keeps one row so it cannot displace live work.
        if options.unfocused
            || options.separate_status_line && !self.has_extension_status()
            || self.active_status_transition().is_some()
        {
            bounds.height = bounds.height.min(1);
        }
        let bounds = self
            .status_content_area(Rect::new(0, 0, bounds.width, bounds.height), options)
            .as_size();
        let height = self.with_status_surface_content(focus, |status| {
            status.map_or(
                u16::from(options.separate_status_line && !options.unfocused),
                |status| status.height(bounds),
            )
        });
        height + if height == 0 { 0 } else { rule }
    }

    pub(super) fn hint_footer_props(&self, _options: ComposerRenderOptions<'_>) -> FooterProps {
        let mut props = self.footer_props();
        if props.status_line_enabled {
            props.status_line_enabled = false;
            props.status_line_value = None;
            props.active_agent_label = None;
        }
        props
    }

    pub(super) fn with_status_surface_content<T>(
        &self,
        focus: StatusLineFocus,
        use_status: impl FnOnce(Option<&StatusLine>) -> T,
    ) -> T {
        if !self.footer.status_line_enabled {
            return use_status(None);
        }
        let status = self.footer.status_line_value.as_ref();
        if status.is_some_and(|status| status.as_native().is_none())
            || self.footer.active_agent_label.is_none()
            || focus == StatusLineFocus::Unfocused
        {
            return use_status(status);
        }
        let mut props = self.footer_props();
        // The viewed agent belongs to native status even during a transient composer hint.
        props.mode = FooterMode::ComposerHasDraft;
        props.is_task_running = false;
        let next = passive_footer_status_line(&props);
        let mut labeled = self.footer.labeled_status_line.borrow_mut();
        // Preserve wrapped rows while the effective line, including the label style, stays equal.
        if *labeled != next {
            *labeled = next;
        }
        use_status(labeled.as_ref())
    }

    fn active_status_transition(&self) -> Option<&EffortStatusLineTransition> {
        self.effort_status_line_transition
            .as_ref()
            .filter(|transition| !transition.is_finished())
    }

    fn has_extension_status(&self) -> bool {
        self.footer
            .status_line_value
            .as_ref()
            .is_some_and(|status| status.as_native().is_none())
    }

    fn status_surface_right(
        &self,
        area: Rect,
        options: ComposerRenderOptions<'_>,
    ) -> Option<Line<'static>> {
        if !options.separate_status_line || options.unfocused || self.has_extension_status() {
            return None;
        }
        let show_cycle_hint = !self.is_task_running
            && !self.blocks_direct_input
            && self.draft.input_enabled
            && !self.popup_active()
            && matches!(
                self.footer_mode(),
                FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
            )
            && options.footer.is_none_or(|footer| !footer.is_interactive);
        let full = self.mode_indicator_line(show_cycle_hint);
        let left = self.with_status_surface_content(StatusLineFocus::Focused, |status| {
            status
                .and_then(StatusLine::as_native)
                .map_or(0, |line| u16::try_from(line.width()).unwrap_or(u16::MAX))
        });
        let right_width = full.as_ref().map_or(0, |line| line.width() as u16);
        if right_width <= inset_footer_hint_area(area).width
            && can_show_left_with_context(area, left, right_width)
        {
            full
        } else {
            self.mode_indicator_line(/*show_cycle_hint*/ false)
        }
    }

    fn status_content_area(&self, area: Rect, options: ComposerRenderOptions<'_>) -> Rect {
        let right = self.status_surface_right(area, options);
        let width =
            max_left_width_for_right(area, right.as_ref().map_or(0, |line| line.width() as u16))
                .unwrap_or_else(|| inset_footer_hint_area(area).width);
        Rect {
            width,
            ..inset_footer_hint_area(area)
        }
    }

    pub(super) fn render_status_surface(
        &self,
        area: Rect,
        buf: &mut Buffer,
        options: ComposerRenderOptions<'_>,
    ) {
        if area.is_empty() {
            return;
        }
        // TELOS(fullscreen-only): persistent context retains its own ruled section.
        let area = if options.separate_status_line && area.height > 1 {
            HorizontalRule.render(area, buf);
            Rect {
                y: area.y + 1,
                height: area.height - 1,
                ..area
            }
        } else {
            area
        };
        let right = self.status_surface_right(area, options);
        let content_area = self.status_content_area(area, options);
        if let Some(right) = right {
            render_context_right(area, buf, &right);
        }
        let area = content_area;
        let focus = options.status_line_focus();
        self.with_status_surface_content(focus, |status| {
            if focus == StatusLineFocus::Focused
                && let Some(transition) = self.active_status_transition()
            {
                let target = status.and_then(StatusLine::as_native).cloned();
                if let Some(frame) = transition.render_line(target, area.width) {
                    StatusLine::from(frame).render(area, buf, focus);
                }
                if let Some(requester) = &self.frame_requester {
                    requester.schedule_frame_in(EFFORT_STATUS_LINE_FRAME_TICK);
                }
            } else if let Some(status) = status {
                status.render(area, buf, focus);
            }
        });
    }
}

impl ComposerRenderOptions<'_> {
    fn status_line_focus(self) -> StatusLineFocus {
        if self.unfocused {
            StatusLineFocus::Unfocused
        } else {
            StatusLineFocus::Focused
        }
    }
}

#[cfg(test)]
#[path = "status_surface_tests.rs"]
mod tests;
