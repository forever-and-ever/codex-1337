//! Shared hint content can live below an inline composer or above the owned transcript.
//! Painting and cursor placement use the same allocated hint rectangle.

use super::*;

pub(in crate::bottom_pane) struct ComposerHints<'a> {
    pub(in crate::bottom_pane) composer: &'a ChatComposer,
    pub(in crate::bottom_pane) options: ComposerRenderOptions<'a>,
}

fn content_area(area: Rect) -> Rect {
    Rect {
        height: area.height.saturating_sub(u16::from(area.height > 1)),
        ..area
    }
}

impl Renderable for ComposerHints<'_> {
    fn desired_height(&self, width: u16) -> u16 {
        self.composer
            .hint_content_height(width, self.options)
            .saturating_add(1)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            self.composer.footer.warning_notice_area.set(None);
            return;
        }
        self.composer
            .render_hints(content_area(area), buf, self.options);
        if area.height > 1 {
            Block::default()
                .borders(ratatui::widgets::Borders::BOTTOM)
                .border_style(Style::default().dim())
                .render(area, buf);
        }
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if area.is_empty() {
            return None;
        }
        self.composer
            .hint_cursor_pos(content_area(area), self.options)
    }

    fn cursor_style(&self, _area: Rect) -> crossterm::cursor::SetCursorStyle {
        crossterm::cursor::SetCursorStyle::SteadyBar
    }
}

impl ChatComposer {
    pub(super) fn hint_cursor_pos(
        &self,
        area: Rect,
        options: ComposerRenderOptions<'_>,
    ) -> Option<(u16, u16)> {
        if let Some(footer) = options.footer.filter(|footer| footer.is_interactive) {
            let area = inset_footer_hint_area(area);
            return footer
                .cursor_column
                .filter(|column| *column < area.width && area.height > 0)
                .map(|column| (area.x + column, area.y));
        }
        self.vim_search_cursor_pos(area)
            .or_else(|| self.history_search_cursor_pos(area))
    }

    pub(super) fn render_hints(
        &self,
        footer_rect: Rect,
        buf: &mut Buffer,
        options: ComposerRenderOptions<'_>,
    ) {
        let transcript_hint_area = inset_footer_hint_area(footer_rect);
        let warning_notice = self.warning_notice_layout(transcript_hint_area, options);
        let warning_area = warning_notice.as_ref().map(|(area, _)| *area);
        match options.footer {
            Some(footer) => {
                self.render_transcript_footer(transcript_hint_area, buf, footer, warning_area);
            }
            None if self.shortcuts_above_composer(options) => {
                let props = self.hint_footer_props(options);
                crate::bottom_pane::shortcut_overlay::close_hint(
                    &props,
                    transcript_hint_area.width,
                )
                .render(transcript_hint_area, buf);
            }
            None => {
                let footer_props = self.hint_footer_props(options);
                let show_cycle_hint = self.status_surface_height(options) == 0
                    && !footer_props.is_task_running
                    && self.footer.collaboration_mode_indicator.is_some();
                let show_shortcuts_hint = match footer_props.mode {
                    FooterMode::ComposerEmpty => !self.is_in_paste_burst(),
                    FooterMode::ComposerHasDraft => false,
                    FooterMode::HistorySearch
                    | FooterMode::QuitShortcutReminder
                    | FooterMode::ShortcutOverlay
                    | FooterMode::EscHint => false,
                };
                let show_queue_hint = match footer_props.mode {
                    FooterMode::ComposerHasDraft => footer_props.is_task_running,
                    FooterMode::HistorySearch
                    | FooterMode::QuitShortcutReminder
                    | FooterMode::ComposerEmpty
                    | FooterMode::ShortcutOverlay
                    | FooterMode::EscHint => false,
                };
                let hint_rect = if let Some(warning_area) = warning_area {
                    Rect {
                        width: warning_area
                            .x
                            .saturating_sub(footer_rect.x)
                            .saturating_sub(/*rhs*/ 2),
                        ..footer_rect
                    }
                } else {
                    footer_rect
                };
                if let Some(input) = self.draft.textarea.vim_query() {
                    input.render(inset_footer_hint_area(hint_rect), buf);
                } else if let Some(line) = self.history_search_footer_line() {
                    render_footer_line(hint_rect, buf, line);
                } else {
                    let available_width =
                        hint_rect.width.saturating_sub(FOOTER_INDENT_COLS as u16) as usize;
                    let status_line_active = uses_passive_footer_status_layout(&footer_props);
                    let combined_status_line = if status_line_active {
                        passive_footer_status_line(&footer_props)
                    } else {
                        None
                    };
                    let transition_visible = status_line_active
                        && !self.footer.flash_visible()
                        && self.footer.hint_override.is_none();
                    let transition_active = transition_visible
                        && self
                            .effort_status_line_transition
                            .as_ref()
                            .is_some_and(|transition| !transition.is_finished());
                    let combined_status_line = if transition_visible
                        && let Some(transition) = &self.effort_status_line_transition
                        && !transition.is_finished()
                    {
                        transition.render_line(
                            combined_status_line.as_ref(),
                            hint_rect.width.saturating_sub(FOOTER_INDENT_COLS as u16),
                        )
                    } else {
                        combined_status_line
                    };
                    let mut truncated_status_line = if status_line_active {
                        combined_status_line.as_ref().map(|line| {
                            truncate_line_with_ellipsis_if_overflow(line.clone(), available_width)
                        })
                    } else {
                        None
                    };
                    let left_mode_indicator =
                        if status_line_active || self.status_surface_height(options) > 0 {
                            None
                        } else {
                            self.footer.collaboration_mode_indicator
                        };
                    let active_footer_hint_override = self.footer.hint_override.as_ref();
                    let mut left_width = if self.footer.flash_visible() {
                        self.footer
                            .flash
                            .as_ref()
                            .map_or(0, |flash| flash.line.width() as u16)
                    } else if let Some(items) = active_footer_hint_override {
                        footer_hint_items_width(items)
                    } else if status_line_active {
                        truncated_status_line
                            .as_ref()
                            .map_or(0, |line| line.width() as u16)
                    } else {
                        footer_line_width(
                            &footer_props,
                            left_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        )
                    };
                    let right_line = if let Some(line) = self.footer.local_shell_notice.as_ref() {
                        Some(line.clone())
                    } else if let Some(label) = self.footer.side_conversation_context_label.as_ref()
                    {
                        Some(side_conversation_context_line(label))
                    } else if let Some(line) = self.shell_mode_footer_line() {
                        Some(line)
                    } else if self.status_surface_height(options) > 0 || transition_active {
                        None
                    } else if status_line_active {
                        let full = self.mode_indicator_line(show_cycle_hint);
                        let compact = self.mode_indicator_line(/*show_cycle_hint*/ false);
                        let full_width = full.as_ref().map_or(0, |l| l.width() as u16);
                        if can_show_left_with_context(hint_rect, left_width, full_width) {
                            full
                        } else {
                            compact
                        }
                    } else {
                        Some(self.right_footer_line_with_context())
                    };
                    let right_width = right_line.as_ref().map_or(0, |l| l.width() as u16);
                    if status_line_active
                        && let Some(max_left) = max_left_width_for_right(hint_rect, right_width)
                        && left_width > max_left
                        && let Some(line) = combined_status_line.as_ref().map(|line| {
                            truncate_line_with_ellipsis_if_overflow(line.clone(), max_left as usize)
                        })
                    {
                        left_width = line.width() as u16;
                        truncated_status_line = Some(line);
                    }
                    let can_show_left_and_context =
                        can_show_left_with_context(hint_rect, left_width, right_width);
                    let has_override =
                        self.footer.flash_visible() || active_footer_hint_override.is_some();
                    let single_line_layout = if has_override || status_line_active {
                        None
                    } else {
                        match footer_props.mode {
                            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft => {
                                // Both of these modes render the single-line footer style (with
                                // either the shortcuts hint or the optional queue hint). We still
                                // want the single-line collapse rules so the mode label can win over
                                // the context indicator on narrow widths.
                                Some(single_line_footer_layout(
                                    hint_rect,
                                    right_width,
                                    left_mode_indicator,
                                    show_cycle_hint,
                                    show_shortcuts_hint,
                                    show_queue_hint,
                                    footer_props.key_hints,
                                ))
                            }
                            FooterMode::EscHint
                            | FooterMode::HistorySearch
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay => None,
                        }
                    };
                    let show_right = if matches!(
                        footer_props.mode,
                        FooterMode::EscHint
                            | FooterMode::HistorySearch
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay
                    ) {
                        false
                    } else {
                        single_line_layout
                            .as_ref()
                            .map_or(can_show_left_and_context, |(_, show_context)| *show_context)
                    };

                    if let Some((summary_left, _)) = single_line_layout {
                        match summary_left {
                            SummaryLeft::Default => {
                                if status_line_active {
                                    if let Some(line) = truncated_status_line.clone() {
                                        render_footer_line(hint_rect, buf, line);
                                    } else {
                                        render_footer_from_props(
                                            hint_rect,
                                            buf,
                                            &footer_props,
                                            left_mode_indicator,
                                            show_cycle_hint,
                                            show_shortcuts_hint,
                                            show_queue_hint,
                                        );
                                    }
                                } else {
                                    render_footer_from_props(
                                        hint_rect,
                                        buf,
                                        &footer_props,
                                        left_mode_indicator,
                                        show_cycle_hint,
                                        show_shortcuts_hint,
                                        show_queue_hint,
                                    );
                                }
                            }
                            SummaryLeft::Custom(line) => {
                                render_footer_line(hint_rect, buf, line);
                            }
                            SummaryLeft::None => {}
                        }
                    } else if self.footer.flash_visible() {
                        if let Some(flash) = self.footer.flash.as_ref() {
                            Widget::render(&flash.line, inset_footer_hint_area(hint_rect), buf);
                        }
                    } else if let Some(items) = active_footer_hint_override {
                        render_footer_hint_items(hint_rect, buf, items);
                    } else if status_line_active {
                        if let Some(line) = truncated_status_line {
                            render_footer_line(hint_rect, buf, line);
                        }
                    } else {
                        render_footer_from_props(
                            hint_rect,
                            buf,
                            &footer_props,
                            self.footer.collaboration_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        );
                    }
                    if show_right && let Some(line) = &right_line {
                        render_context_right(hint_rect, buf, line);
                    }
                    if status_line_active
                        && let Some(url) = self.footer.status_line_hyperlink_url.as_deref()
                    {
                        mark_underlined_hyperlink(buf, hint_rect, url);
                    }
                    if transition_visible
                        && let Some(transition) = &self.effort_status_line_transition
                        && !transition.is_finished()
                        && let Some(frame_requester) = &self.frame_requester
                    {
                        frame_requester.schedule_frame_in(EFFORT_STATUS_LINE_FRAME_TICK);
                    }
                }
            }
        }
        self.footer.warning_notice_area.set(warning_area);
        if let Some((warning_area, line)) = warning_notice {
            line.render(warning_area, buf);
        }
    }
}
