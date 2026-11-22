//! Shared hint content can live below an inline composer or above the owned transcript.
//! Painting and cursor placement use the same allocated hint rectangle.

use super::Block;
use super::Buffer;
use super::ChatComposer;
use super::ComposerRenderOptions;
use super::FooterMode;
use super::Rect;
use super::Renderable;
use super::Style;
use super::Widget;
use super::can_show_left_with_context;
use super::footer_hint_items_width;
use super::footer_line_width;
use super::inset_footer_hint_area;
use super::passive_hints;
use super::render_context_right;
use super::render_footer_from_props;
use super::render_footer_hint_items;
use super::render_footer_line;
use super::side_conversation_context_line;
use super::warning_notice;
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
        let content = content_area(area);
        self.composer.render_hints(content, buf, self.options);
        self.composer
            .register_rendered_selection([content.intersection(buf.area)], self.options);
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
        if options.unfocused || options.is_dialog_backdrop {
            return None;
        }
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
        if options.is_dialog_backdrop {
            self.footer.warning_notice_area.set(None);
            return;
        }
        let transcript_hint_area = inset_footer_hint_area(footer_rect);
        let passive = self.passive_hints(options);
        let warning_notice = if !options.unfocused && passive.is_none() {
            self.warning_notice_layout(transcript_hint_area, options)
        } else {
            None
        };
        let warning_area = warning_notice.as_ref().map(|(area, _)| *area);
        let mut painted_warning = warning_area;
        match (options.footer, passive) {
            _ if options.unfocused => {}
            (_, Some(hints)) => {
                let hint_area = passive_hints::hint_area(footer_rect);
                painted_warning = hints.render(hint_area, buf);
            }
            (Some(footer), None) => {
                self.render_transcript_footer(
                    transcript_hint_area,
                    buf,
                    footer,
                    warning_area,
                    options,
                );
            }
            (None, None) if self.shortcuts_above_composer(options) => {
                let props = self.hint_footer_props(options);
                crate::bottom_pane::shortcut_overlay::close_hint(
                    &props,
                    transcript_hint_area.width,
                )
                .render(transcript_hint_area, buf);
            }
            (None, None) => {
                let hint_rect = warning_notice::before_warning(footer_rect, warning_area);
                if let Some(input) = self.draft.textarea.vim_query() {
                    input.render(inset_footer_hint_area(hint_rect), buf);
                } else if let Some(line) = self.history_search_footer_line() {
                    render_footer_line(hint_rect, buf, line);
                } else {
                    let footer_props = self.hint_footer_props(options);
                    let show_cycle_hint = !self.popup_active()
                        && !self.blocks_direct_input
                        && self.draft.input_enabled
                        && !footer_props.is_task_running
                        && self.footer.collaboration_mode_indicator.is_some();
                    let show_shortcuts_hint =
                        footer_props.mode == FooterMode::ComposerEmpty && !self.is_in_paste_burst();
                    let show_queue_hint = footer_props.mode == FooterMode::ComposerHasDraft
                        && footer_props.is_task_running;
                    let status_line_active = self.footer.status_line_enabled;
                    let left_mode_indicator = if status_line_active {
                        None
                    } else {
                        self.footer.collaboration_mode_indicator
                    };
                    let active_footer_hint_override = self.footer.hint_override.as_ref();
                    let left_width = if self.footer.flash_visible() {
                        self.footer.flash.as_ref().map_or(0, |flash| {
                            u16::try_from(flash.line.width()).unwrap_or(u16::MAX)
                        })
                    } else if let Some(items) = active_footer_hint_override {
                        footer_hint_items_width(items)
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
                    } else if status_line_active
                        && options.separate_status_line
                        && self
                            .footer
                            .status_line_value
                            .as_ref()
                            .is_none_or(|line| line.as_native().is_some())
                    {
                        None
                    } else if status_line_active {
                        let full = self.mode_indicator_line(show_cycle_hint);
                        let compact = self.mode_indicator_line(/*show_cycle_hint*/ false);
                        let full_width = full
                            .as_ref()
                            .map_or(0, |line| u16::try_from(line.width()).unwrap_or(u16::MAX));
                        if can_show_left_with_context(hint_rect, left_width, full_width) {
                            full
                        } else {
                            compact
                        }
                    } else {
                        Some(self.right_footer_line_with_context())
                    };
                    let right_width = right_line
                        .as_ref()
                        .map_or(0, |line| u16::try_from(line.width()).unwrap_or(u16::MAX));
                    let can_show_left_and_context =
                        can_show_left_with_context(hint_rect, left_width, right_width);
                    let has_override =
                        self.footer.flash_visible() || active_footer_hint_override.is_some();
                    let passive_mode = matches!(
                        footer_props.mode,
                        FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
                    );
                    let show_right = if has_override && status_line_active {
                        can_show_left_and_context
                    } else {
                        passive_mode && (!has_override || can_show_left_and_context)
                    };

                    if self.footer.flash_visible() {
                        if let Some(flash) = self.footer.flash.as_ref() {
                            Widget::render(&flash.line, inset_footer_hint_area(hint_rect), buf);
                        }
                    } else if let Some(items) = active_footer_hint_override {
                        render_footer_hint_items(hint_rect, buf, items);
                    } else if !passive_mode {
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
                    if show_right && let Some(line) = &right_line {
                        render_context_right(hint_rect, buf, line);
                    }
                }
            }
        }
        self.footer.warning_notice_area.set(painted_warning);
        if let Some((warning_area, line)) = warning_notice {
            line.render(warning_area, buf);
        }
    }

    pub(super) fn register_rendered_selection(
        &self,
        areas: impl IntoIterator<Item = Rect>,
        options: ComposerRenderOptions<'_>,
    ) {
        if let Some(selection) = options.rendered_selection
            && !self.popup_active()
            && options.footer.is_none_or(|footer| !footer.is_interactive)
        {
            let revision = (options.warning_count, self.footer_props().is_task_running);
            let mut selection = selection.borrow_mut();
            if self.footer.selection_revision.replace(revision) != revision {
                selection.clear();
            }
            for area in areas {
                selection.register(area, false);
            }
        }
    }
}
