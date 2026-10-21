//! Patches: cells painted over the last full frame, which otherwise stands.
//!
//! A patch writes exactly what a full frame would for the rows it covers, measured against the
//! screen beneath it, and leaves that screen patched so the next full frame diffs from the truth.

use std::io;
use std::io::Write;

use crossterm::cursor::SetCursorStyle;
use ratatui::backend::Backend;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;

use super::DrawCommand;
use super::Terminal;
use super::diff_buffers;
use super::output;

impl<B> Terminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    /// Whether raw output cleared or moved screen content since the last full frame.
    pub(crate) fn output_invalidated(&self) -> bool {
        self.output_invalidation.is_marked()
    }

    /// Paints `cells` over the last full frame.
    pub(crate) fn draw_patch(
        &mut self,
        mut cells: Buffer,
        cursor: Option<Position>,
        style: SetCursorStyle,
    ) -> io::Result<()> {
        // TELOS(warpaint): cells hold tokens; a patch binds to the pack worn, as a frame does.
        crate::warpaint::bind_frame(&mut cells, &mut self.backend)?;
        if output::capture_patch(
            &cells,
            self.last_known_screen_size,
            self.viewport_area,
            cursor,
            style,
        ) {
            overlay(self.previous_buffer_mut(), &cells);
            return Ok(());
        }
        self.paint_patch(&cells, cursor, style)
    }

    /// Writes what painting `cells` over the screen changes, then leaves the cursor as the frame
    /// beneath them did.
    pub(super) fn paint_patch(
        &mut self,
        cells: &Buffer,
        cursor: Option<Position>,
        style: SetCursorStyle,
    ) -> io::Result<()> {
        let updates = diff_rows(self.previous_buffer(), cells);
        overlay(self.previous_buffer_mut(), cells);
        // The patched screen stands as the current frame while a cursor style repairs its anchor.
        self.current = 1 - self.current;
        let presented = self.present(updates, cursor, style);
        self.current = 1 - self.current;
        presented?;
        Backend::flush(&mut self.backend)
    }

    /// Writes `updates`, then shows the cursor where and as the frame wants it, or hides it.
    pub(super) fn present(
        &mut self,
        updates: Vec<DrawCommand>,
        cursor: Option<Position>,
        style: SetCursorStyle,
    ) -> io::Result<()> {
        if !updates.is_empty() && !self.hidden_cursor {
            self.hide_cursor()?;
        }
        self.flush_updates(updates)?;
        match cursor {
            None if !self.hidden_cursor => self.hide_cursor(),
            None => Ok(()),
            Some(position) => {
                self.set_cursor_style_with_repair(style)?;
                self.set_cursor_position(position)?;
                self.show_cursor()
            }
        }
    }
}

/// What a full frame writes for the rows `cells` covers, over `screen`.
fn diff_rows(screen: &Buffer, cells: &Buffer) -> Vec<DrawCommand> {
    let rows = Rect {
        y: cells.area.y,
        height: cells.area.height,
        ..screen.area
    }
    .intersection(screen.area);
    let mut before = Buffer::empty(rows);
    overlay(&mut before, screen);
    let mut after = before.clone();
    overlay(&mut after, cells);
    diff_buffers(&before, &after)
}

/// Copies onto `screen` the cells of `cells` that fall on it.
fn overlay(screen: &mut Buffer, cells: &Buffer) {
    for position in cells.area.intersection(screen.area).positions() {
        screen[position] = cells[position].clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_backend::VT100Backend;
    use ratatui::style::Color;
    use ratatui::style::Modifier;
    use ratatui::style::Style;

    const SCREEN: Rect = Rect::new(0, 0, 26, 3);
    const STATUS: Rect = Rect::new(2, 1, 24, 1);
    const CURSOR: Position = Position::new(10, 2);

    fn frame(status: &str) -> Buffer {
        let mut buffer = Buffer::empty(SCREEN);
        buffer.set_string(0, 0, "transcript", Style::default().fg(Color::Blue));
        buffer.set_string(
            STATUS.x,
            STATUS.y,
            status,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        );
        buffer.set_string(0, 2, "› compose", Style::default());
        buffer
    }

    fn draw(terminal: &mut Terminal<VT100Backend>, buffer: Buffer) -> io::Result<()> {
        terminal.draw(|frame| {
            *frame.buffer_mut() = buffer;
            frame.set_cursor_style(SetCursorStyle::SteadyBar);
            frame.set_cursor_position(CURSOR);
        })
    }

    fn screen(terminal: &Terminal<VT100Backend>) -> (Vec<u8>, (u16, u16), bool) {
        let screen = terminal.backend().vt100().screen();
        (
            screen.contents_formatted(),
            screen.cursor_position(),
            screen.hide_cursor(),
        )
    }

    #[test]
    fn a_patch_leaves_the_screen_a_full_frame_would() -> io::Result<()> {
        let mut patched = Terminal::with_options(VT100Backend::new(SCREEN.width, SCREEN.height))?;
        let mut full = Terminal::with_options(VT100Backend::new(SCREEN.width, SCREEN.height))?;
        for terminal in [&mut patched, &mut full] {
            terminal.set_viewport_area(SCREEN);
            draw(terminal, frame("⠋ Working (59s • esc)"))?;
        }
        // The shorter row must clear the tail the longer one left.
        let next = frame("⠙ Working (1m)");
        draw(&mut full, next.clone())?;
        let mut cells = Buffer::empty(STATUS);
        overlay(&mut cells, &next);
        patched.draw_patch(cells, Some(CURSOR), SetCursorStyle::SteadyBar)?;
        assert_eq!(screen(&patched), screen(&full));
        // The next full frame diffs against the patched screen.
        for terminal in [&mut patched, &mut full] {
            draw(terminal, frame("⠹ Working (1m)"))?;
        }
        assert_eq!(screen(&patched), screen(&full));
        Ok(())
    }
}
