//! Liveness is paint over immutable header geometry, never captured text or a layout revision.

use super::HeaderRows;
use super::TextLayout;
use super::ToolMark;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::transcript_view::TranscriptView;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

impl HeaderRows {
    pub(in crate::transcript_view) fn with_tool_mark(
        mut self,
        motion: Option<MotionMode>,
        width: u16,
    ) -> Self {
        let Some(motion) = motion else {
            return self;
        };
        let Some(line) = self.lines.first() else {
            return self;
        };
        let Some(source) = &line.source else {
            return self;
        };
        let text = line.line.to_string();
        let Some(label) = text.get(..source.prefix_bytes + source.range.len()) else {
            return self;
        };
        let column = label.width() + 1;
        let limit = self
            .controls
            .iter()
            .filter(|control| control.row == 0)
            .map(|control| control.columns.start)
            .min()
            .unwrap_or(width)
            .min(width);
        // Use existing title padding. A narrow header clips the mark, never a control or title.
        if column < usize::from(limit)
            && let Ok(column) = u16::try_from(column)
        {
            self.tool_mark = Some(ToolMark {
                row: 0,
                column,
                motion,
            });
        }
        self
    }
}

impl TextLayout {
    pub(in crate::transcript_view) fn with_tool_mark(mut self, mut mark: ToolMark) -> Self {
        if let Some(row) = self.row_for_input_line(mark.row) {
            mark.row = row;
            self.heading.tool_marks.push(mark);
        }
        self
    }

    pub(super) fn rewrap_tool_marks(&self, layout: &mut Self, header_lines: usize) {
        for mark in &self.heading.tool_marks {
            let Some(line) = self.rows[mark.row]
                .logical_line
                .and_then(|line| line.checked_sub(header_lines))
            else {
                continue;
            };
            if let Some(row) = layout
                .rows
                .iter()
                .position(|row| row.logical_line == Some(line))
            {
                layout.heading.tool_marks.push(ToolMark { row, ..*mark });
            }
        }
    }

    pub(in crate::transcript_view) fn has_animated_tool_mark(&self, row: usize) -> bool {
        self.heading.tool_marks.iter().any(|mark| {
            mark.row == row && mark.column < self.width && mark.motion == MotionMode::Animated
        })
    }

    pub(in crate::transcript_view) fn paint_tool_marks(
        &self,
        area: Rect,
        buf: &mut Buffer,
        start_row: usize,
        motion: MotionMode,
    ) {
        for mark in &self.heading.tool_marks {
            let Some(row) = mark
                .row
                .checked_sub(start_row)
                .and_then(|row| u16::try_from(row).ok())
                .filter(|row| *row < area.height)
            else {
                continue;
            };
            if mark.column >= area.width {
                continue;
            }
            let mode = if motion == MotionMode::Animated {
                mark.motion
            } else {
                MotionMode::Reduced
            };
            if let Some(span) =
                crate::motion::activity_indicator(None, mode, ReducedMotionIndicator::StaticBullet)
            {
                buf.set_span(area.x + mark.column, area.y + row, &span, 1);
            }
        }
    }
}

impl TranscriptView {
    pub(in crate::transcript_view) fn paint_tool_marks(&self, buf: &mut Buffer) {
        let motion = if self.is_following()
            && self.snapshot().is_none()
            && self
                .cache
                .frames
                .as_ref()
                .is_none_or(|frames| frames.mode() == crate::tui::PresentationMode::Normal)
        {
            MotionMode::Animated
        } else {
            MotionMode::Reduced
        };
        for (visible, y) in self.visible.iter().zip(self.area.top()..self.area.bottom()) {
            visible.layout.paint_tool_marks(
                Rect::new(self.area.x, y, self.area.width, 1),
                buf,
                visible.row,
                motion,
            );
        }
    }
}
