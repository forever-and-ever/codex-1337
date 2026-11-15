//! Shared composer geometry keeps rendering, measurement, and the cursor aligned.

use super::*;
use crate::bottom_pane::voice_strip::VoiceStrip;
use crate::bottom_pane::voice_strip::VoiceStripState;
use crate::render::renderable::Renderable;
use crate::tui::FrameRequester;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

pub(super) const MAX_FOOTER_ROWS: u16 = 4;

struct ComposerRows {
    input: u16,
    above_input: u16,
    extension: u16,
    status: u16,
    hints: u16,
}

impl ChatComposer {
    pub(crate) fn set_voice_strip(
        &mut self,
        state: Option<VoiceStripState>,
        frame_requester: FrameRequester,
    ) {
        match (self.voice_strip.as_mut(), state) {
            (Some(strip), Some(state)) => strip.update(state),
            (_, Some(state)) => self.voice_strip = Some(VoiceStrip::new(state, frame_requester)),
            (_, None) => self.voice_strip = None,
        }
    }

    pub(super) fn render_voice_strip(&self, composer_rect: Rect, buf: &mut Buffer) {
        if let Some(strip) = &self.voice_strip
            && composer_rect.height >= strip.desired_height(composer_rect.width) + 4
        {
            let voice_rect = Rect {
                y: composer_rect.y.saturating_add(/*rhs*/ 1),
                height: strip.desired_height(composer_rect.width),
                ..composer_rect
            };
            strip.render(voice_rect, buf);
        }
    }
}

impl ChatComposer {
    pub(super) fn layout_with_options(
        &self,
        area: Rect,
        options: ComposerRenderOptions<'_>,
    ) -> ComposerLayout {
        let ComposerRows {
            above_input,
            extension: extension_height,
            status: status_height,
            hints: footer_hint_height,
            ..
        } = self.composer_rows(area.width, area.height, options);
        let status = Rect {
            y: area
                .bottom()
                .saturating_sub(footer_hint_height)
                .saturating_sub(status_height)
                .max(area.y),
            height: status_height,
            ..area
        };
        let extension = Rect {
            y: status.y.saturating_sub(extension_height),
            height: extension_height,
            ..area
        };
        let area = Rect {
            height: area.height - status_height - extension_height,
            ..area
        };
        let voice_rows = self.voice_rows(area.width);
        let composer_min = self
            .composer_min_height(area.width)
            .saturating_add(voice_rows);
        let shortcuts_above = self.shortcuts_above_composer(options);
        let (composer_rect, popup_rect, mut footer_rect) = if shortcuts_above {
            let [shortcuts, composer, footer] = Layout::vertical([
                Constraint::Max(above_input),
                Constraint::Min(composer_min),
                Constraint::Length(footer_hint_height),
            ])
            .areas(area);
            (composer, shortcuts, footer)
        } else if self.popups.active.is_above_composer() {
            // Preserve the draft first, then its footer, and use remaining space for suggestions.
            let [mut composer, mut popup, footer] = Layout::vertical([
                Constraint::Min(composer_min),
                Constraint::Max(above_input),
                Constraint::Length(footer_hint_height),
            ])
            .areas(area);
            popup.y = area.y;
            composer.y = popup.bottom();
            (composer, popup, footer)
        } else {
            let [composer, popup] = Layout::vertical([
                Constraint::Min(composer_min),
                Constraint::Max(footer_hint_height),
            ])
            .areas(area);
            (composer, popup, popup)
        };
        footer_rect.y = footer_rect
            .y
            .saturating_add(status.height + extension.height);
        // Keep the draft visible when clipped.
        let voice_rows = voice_rows * u16::from(composer_rect.height >= voice_rows + 3);
        let mut textarea_rect = composer_rect.inset(Insets::tlbr(
            /*top*/ 1 + voice_rows,
            LIVE_PREFIX_COLS,
            /*bottom*/ 1,
            /*right*/ 1,
        ));
        let remote_images_height = self
            .attachments
            .remote_image_lines()
            .len()
            .try_into()
            .unwrap_or(u16::MAX)
            .min(textarea_rect.height.saturating_sub(2));
        let remote_images_separator = u16::from(remote_images_height > 0);
        let consumed = remote_images_height.saturating_add(remote_images_separator);
        let remote_images_rect = Rect {
            x: textarea_rect.x,
            y: textarea_rect.y,
            width: textarea_rect.width,
            height: remote_images_height,
        };
        textarea_rect.y = textarea_rect.y.saturating_add(consumed);
        textarea_rect.height = textarea_rect.height.saturating_sub(consumed);
        ComposerLayout {
            status,
            extension,
            composer: composer_rect,
            remote_images: remote_images_rect,
            textarea: textarea_rect,
            popup: popup_rect,
            footer: footer_rect,
        }
    }

    pub(crate) fn cursor_pos_with_options(
        &self,
        area: Rect,
        options: ComposerRenderOptions<'_>,
    ) -> Option<(u16, u16)> {
        let layout = self.layout_with_options(area, options);
        if options.footer.is_some_and(|footer| footer.is_interactive) {
            return match options.hint_placement {
                ComposerHintPlacement::BelowComposer => {
                    self.hint_cursor_pos(layout.footer, options)
                }
                ComposerHintPlacement::ScreenHeader => None,
            };
        }
        if !self.draft.input_enabled
            || (self.blocks_direct_input && !self.child_text_input_enabled)
            || self.attachments.selected_remote_image_index.is_some()
        {
            return None;
        }

        if self.draft.textarea.vim_query().is_some() || self.history_search.is_some() {
            return match options.hint_placement {
                ComposerHintPlacement::BelowComposer => {
                    self.hint_cursor_pos(layout.footer, options)
                }
                ComposerHintPlacement::ScreenHeader => None,
            };
        }

        let state = *self.draft.textarea_state.borrow();
        self.draft
            .textarea
            .cursor_pos_with_state(layout.textarea, state)
    }

    pub(crate) fn desired_height_with_options(
        &self,
        width: u16,
        options: ComposerRenderOptions<'_>,
    ) -> u16 {
        let rows = self.composer_rows(width, options.max_height.unwrap_or(u16::MAX), options);
        [
            rows.input,
            rows.above_input,
            rows.extension,
            rows.status,
            rows.hints,
        ]
        .into_iter()
        .fold(0, u16::saturating_add)
    }

    fn voice_rows(&self, width: u16) -> u16 {
        if self.blocks_direct_input && !self.child_text_input_enabled {
            0
        } else {
            self.voice_strip
                .as_ref()
                .map_or(0, |strip| strip.desired_height(width).saturating_add(1))
        }
    }

    fn composer_min_height(&self, width: u16) -> u16 {
        if self.blocks_direct_input && !self.child_text_input_enabled {
            super::super::watch_help::lines(width).len() as u16
        } else if self.attachments.remote_image_urls.is_empty() {
            3
        } else {
            // An attachment, its separator, and the editable prompt survive before extensions.
            5
        }
    }

    fn composer_rows(
        &self,
        width: u16,
        height: u16,
        options: ComposerRenderOptions<'_>,
    ) -> ComposerRows {
        let inner_width = width.saturating_sub(LIVE_PREFIX_COLS + 1);
        let input = if self.blocks_direct_input && !self.child_text_input_enabled {
            self.composer_min_height(width)
        } else {
            let remote: u16 = self
                .attachments
                .remote_image_lines()
                .len()
                .try_into()
                .unwrap_or(u16::MAX);
            [
                self.draft.textarea.desired_height(inner_width),
                remote,
                u16::from(remote > 0),
                2,
                self.voice_rows(width),
            ]
            .into_iter()
            .fold(0, u16::saturating_add)
        };
        let minimum = self
            .composer_min_height(width)
            .saturating_add(self.voice_rows(width));
        let wanted_hints = self.footer_hint_height(width, options);
        let find_active = options
            .footer
            .is_some_and(|footer| footer.cursor_column.is_some());
        let reserved_hints = wanted_hints.min(if find_active { 2 } else { 1 });
        let has_required_hints = find_active || matches!(
            self.footer_mode(),
            FooterMode::HistorySearch | FooterMode::ShortcutOverlay
                | FooterMode::QuitShortcutReminder | FooterMode::EscHint
        ) || options.footer.is_some_and(|footer| footer.is_interactive)
            // Inline warnings outrank optional status; native status keeps its ruled minimum.
            || (!options.separate_status_line && self.show_warning_notice(options));
        let required_hints = if has_required_hints {
            reserved_hints
        } else {
            0
        };
        // TELOS(fullscreen-only): keep the native status rule even when the draft scrolls.
        let basic_status = u16::from(self.footer.status_line_enabled)
            * (1 + u16::from(options.separate_status_line));
        let room = height.saturating_sub(minimum);
        let basic_status = basic_status.min(room.saturating_sub(required_hints));
        let extension = options.extension.map_or(0, |extension| {
            extension
                .desired_height(width)
                .min(room.saturating_sub(required_hints.saturating_add(basic_status)))
        });
        let mut available = height
            .saturating_sub(input)
            .saturating_sub(extension)
            .max(required_hints.saturating_add(basic_status))
            .min(room.saturating_sub(extension));
        let budget = options.max_height.unwrap_or(u16::MAX);
        let cap = if options.separate_status_line {
            (budget.saturating_add(2) / 4)
                .clamp(2, MAX_FOOTER_ROWS)
                .saturating_add(1)
        } else {
            2
        };
        let wanted_status = self.status_surface_height(
            width,
            available.min(cap).saturating_sub(
                required_hints
                    .max(reserved_hints.min(available.min(cap).saturating_sub(basic_status))),
            ),
            options,
        );
        let above_input = if self.shortcuts_above_composer(options) {
            footer_height(&self.hint_footer_props(options), width)
        } else if options.command_popup_placement == CommandPopupPlacement::AboveComposer {
            self.popups
                .active
                .required_height(width, /*footer_total_height*/ 0)
        } else {
            0
        }
        .min(available.saturating_sub(reserved_hints.saturating_add(basic_status)));
        available = available.saturating_sub(above_input);
        let limit = cap.min(available);
        let reserved_status = wanted_status
            .min(basic_status)
            .min(limit.saturating_sub(required_hints));
        let hints = wanted_hints.min(limit.saturating_sub(reserved_status));
        ComposerRows {
            input,
            above_input,
            extension,
            status: wanted_status.min(limit.saturating_sub(hints)),
            hints,
        }
    }
}
