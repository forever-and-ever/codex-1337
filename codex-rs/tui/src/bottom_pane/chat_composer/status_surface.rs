//! Separate persistent transcript context from the composer's transient hint row.
//! Measurement, rendering, and cursor placement share the same reserved status rectangle.
//! Shortcut help grows above the input, preserving status and close rows below it even when clipped.

use super::*;

use crate::bottom_pane::footer::max_left_width_for_right;
use crate::render::ruled_sections::HorizontalRule;
use std::borrow::Cow;

impl ChatComposer {
    pub(crate) fn set_status_line(&mut self, status_line: Option<FooterContent>) -> bool {
        if self.footer.status_line_value == status_line {
            return false;
        }
        if matches!(status_line, Some(FooterContent::Extension { .. })) {
            self.effort_status_line_transition = None;
        }
        self.footer.measured_status_line.set(None);
        self.footer.wrapped_native_status.take();
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
        width: u16,
        available: u16,
        options: ComposerRenderOptions<'_>,
    ) -> u16 {
        if !self.footer.status_line_enabled || available == 0 {
            return 0;
        }
        let rule = u16::from(options.separate_status_line && available > 1);
        // TELOS(fullscreen-only): native context keeps one row; extra rows cannot displace live work or header notices.
        let available = if options.separate_status_line
            && !matches!(
                self.footer.status_line_value,
                Some(FooterContent::Extension { .. })
            ) {
            available.saturating_sub(rule).min(1)
        } else {
            available.saturating_sub(rule)
        };
        // The unfocused composer paints only the raw status, as one dim line.
        let (available, status) = if options.unfocused {
            let raw = self
                .status_line_content()
                .filter(|_| self.footer.status_line_enabled);
            (available.min(1), raw.map(Cow::Borrowed))
        } else {
            (available, self.status_surface_content())
        };
        let bounds = self
            .status_content_area(Rect::new(0, 0, width, available), options)
            .as_size();
        if bounds.width == 0 || bounds.height == 0 {
            return 0;
        }
        let height = match status.as_deref() {
            Some(FooterContent::Extension { content }) => {
                let cache = &self.footer.measured_status_line;
                if let Some((previous, height)) = cache.get()
                    && previous == bounds
                {
                    return height.saturating_add(rule);
                }
                let height = content.measure(bounds).height;
                cache.set(Some((bounds, height)));
                height
            }
            Some(native) if bounds.height > 1 && self.active_status_transition().is_none() => self
                .wrapped_native_status(native, bounds.width)
                .map_or_else(
                    || u16::from(native.width() > 0),
                    |rows| rows.height().min(bounds.height),
                ),
            Some(line) => u16::from(line.width() > 0),
            None => u16::from(options.separate_status_line && !options.unfocused),
        };
        if height == 0 {
            0
        } else {
            height.saturating_add(rule)
        }
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

    pub(super) fn status_surface_content(&self) -> Option<Cow<'_, FooterContent>> {
        if !self.footer.status_line_enabled {
            return None;
        }
        let status = self.footer.status_line_value.as_ref();
        if matches!(status, Some(FooterContent::Extension { .. }))
            || self.footer.active_agent_label.is_none()
        {
            return status.map(Cow::Borrowed);
        }
        let mut props = self.footer_props();
        // The viewed agent belongs to native status even during a transient composer hint.
        props.mode = FooterMode::ComposerHasDraft;
        props.is_task_running = false;
        passive_footer_status_line(&props).map(Cow::Owned)
    }

    fn active_status_transition(&self) -> Option<&EffortStatusLineTransition> {
        self.effort_status_line_transition
            .as_ref()
            .filter(|transition| !transition.is_finished())
    }

    fn status_surface_right(
        &self,
        area: Rect,
        options: ComposerRenderOptions<'_>,
    ) -> Option<Line<'static>> {
        if !options.separate_status_line
            || options.unfocused
            || matches!(
                self.footer.status_line_value,
                Some(FooterContent::Extension { .. })
            )
        {
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
        let left = self
            .status_surface_content()
            .as_deref()
            .map_or(0, FooterContent::width);
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
        if options.unfocused {
            if let Some(content) = self.status_line_content() {
                crate::bottom_pane::footer::render_unfocused_status(
                    content.clone().into_hyperlink_line().line.to_string(),
                    area,
                    buf,
                );
            }
            return;
        }
        let right = self.status_surface_right(area, options);
        let content_area = self.status_content_area(area, options);
        if let Some(right) = right {
            render_context_right(area, buf, &right);
        }
        let area = content_area;
        let line = self.status_surface_content();
        if let Some(transition) = self.active_status_transition() {
            if let Some(frame) = transition.render_line(
                line.map(|line| line.into_owned().into_hyperlink_line()),
                area.width,
            ) {
                FooterContent::HyperlinkLine(frame).render(area, buf);
            }
            if let Some(requester) = &self.frame_requester {
                requester.schedule_frame_in(EFFORT_STATUS_LINE_FRAME_TICK);
            }
        } else if let Some(line) = line {
            if area.height > 1
                && let Some(wrapped) = self.wrapped_native_status(&line, area.width)
            {
                wrapped.render(area, buf);
            } else {
                line.into_owned().render(area, buf);
            }
        }
    }
}

#[cfg(test)]
#[path = "status_surface_tests.rs"]
mod tests;
