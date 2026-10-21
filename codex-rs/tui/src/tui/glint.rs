//! Glints: the status row's motion, painted alone between full frames.
//!
//! A full frame offers a glint when the status row it painted reaches the frame's end intact. A
//! glint repaints that row over the cells the frame laid beneath it and writes only what changed,
//! so a working agent's shimmer costs a row rather than a screen. Everything else the screen owes
//! takes a full frame.

use std::cell::RefCell;
use std::io;
use std::time::Duration;
use std::time::Instant;

use crossterm::cursor::SetCursorStyle;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::layout::Size;

use super::Tui;
use super::frame_requester::Due;
use crate::custom_terminal::output::stdout;
use crate::ui_latency;

/// A focused window still paints fully this often, so clocks beyond the status row keep time.
pub(super) const FULL_FRAME_FLOOR: Duration = Duration::from_secs(1);

thread_local! {
    /// The status row the frame in progress painted.
    static PAINTED: RefCell<Option<Painted>> = const { RefCell::new(None) };
}

struct Painted {
    /// The cells the frame laid beneath the row before the row painted.
    base: Buffer,
    /// The row as it painted.
    row: Buffer,
    /// The row's height in lines, which its glint must keep.
    height: u16,
}

/// A full frame's status row, ready to glint over that frame.
pub(crate) struct Offer {
    base: Buffer,
    height: u16,
    cursor: Option<Position>,
    style: SetCursorStyle,
}

/// What may glint, and when the screen last painted fully.
#[derive(Default)]
pub(crate) struct Glint {
    held: Option<Held>,
    painted_at: Option<Instant>,
}

/// An offer, with the geometry of the frame that made it.
struct Held {
    offer: Offer,
    screen: Size,
    viewport: Rect,
}

/// Paints the status row through `paint`, recording it, `height` lines tall, for the frame's offer.
pub(crate) fn paint_status(
    area: Rect,
    height: u16,
    buf: &mut Buffer,
    paint: impl FnOnce(&mut Buffer),
) {
    let area = area.intersection(buf.area);
    let base = cells(buf, area);
    paint(buf);
    if !area.is_empty() {
        PAINTED.set(Some(Painted {
            base,
            row: cells(buf, area),
            height,
        }));
    }
}

/// Ends a full frame's painting with its offer, when its status row reached the end intact.
pub(crate) fn settle(
    buf: &Buffer,
    cursor: Option<Position>,
    style: SetCursorStyle,
) -> Option<Offer> {
    let Painted { base, row, height } = PAINTED.take()?;
    row.area
        .positions()
        .all(|position| buf.cell(position) == row.cell(position))
        .then_some(Offer {
            base,
            height,
            cursor,
            style,
        })
}

fn cells(buf: &Buffer, area: Rect) -> Buffer {
    let mut cells = Buffer::empty(area);
    for position in area.positions() {
        cells[position] = buf[position].clone();
    }
    cells
}

impl Tui {
    /// Starts a full frame, which pays every debt and withdraws the standing offer.
    pub(super) fn begin_full_frame(&mut self) {
        self.frame_requester.pay_all();
        PAINTED.take();
        self.glint = Glint {
            held: None,
            painted_at: Some(Instant::now()),
        };
    }

    /// Holds the offer the full frame just drawn settled on.
    pub(crate) fn offer_glint(&mut self, offer: Option<Offer>) {
        self.glint.held = offer.map(|offer| Held {
            offer,
            screen: self.terminal.last_known_screen_size,
            viewport: self.terminal.viewport_area,
        });
    }

    pub(super) fn painted_fully_within(&self, span: Duration) -> bool {
        self.glint.painted_at.is_some_and(|at| at.elapsed() < span)
    }

    /// Repaints the held status row through `paint`, which declines when the row's layout moved,
    /// and writes what changed. False when only a full frame can pay what the screen owes.
    pub(crate) fn draw_glint(
        &mut self,
        screen_size: Size,
        paint: impl FnOnce(Rect, u16, &mut Buffer) -> bool,
    ) -> io::Result<bool> {
        self.frame_requester.pay(Due::GLINT);
        let Some(held) = &self.glint.held else {
            // No status row stands on screen, so its motion has nothing to show.
            return Ok(true);
        };
        if held.screen != screen_size
            || held.viewport != self.terminal.viewport_area
            || self.terminal.output_invalidated()
            || self.clear_thread_switch_on_draw
            || !self.pending_history_lines.is_empty()
            || !self.owned_screen
            || !self.is_alt_screen_active()
        {
            return Ok(false);
        }
        let _paint_stage = ui_latency::enter(ui_latency::Stage::GlintPaint);
        let mut cells = held.offer.base.clone();
        let (height, cursor, style) = (held.offer.height, held.offer.cursor, held.offer.style);
        let painted = paint(cells.area, height, &mut cells);
        PAINTED.take();
        if !painted {
            return Ok(false);
        }
        let terminal = &mut self.terminal;
        stdout().sync_update(|_| terminal.draw_patch(cells, cursor, style))??;
        Ok(true)
    }
}
