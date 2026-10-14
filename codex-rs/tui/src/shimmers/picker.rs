//! `/shimmer`: the stock roster beside a live preview of the highlighted shimmer.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

use super::ROSTER;
use super::engine::Engine;
use super::engine::color;
use super::live;
use super::script::Script;
use super::stage::Palette;
use super::stage::TICK;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::SideContentWidth;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::motion::MotionMode;
use crate::render::renderable::Renderable;
use crate::status_indicator_widget::fmt_elapsed_compact;
use crate::tui::FrameRequester;

/// Columns the preview needs beside the list.
const PREVIEW_WIDTH: u16 = 44;
/// Rows of the preview: a running tool and its output, a blank row, the status row.
const PREVIEW_ROWS: u16 = 4;

pub(crate) fn picker(frames: FrameRequester, motion: MotionMode) -> SelectionViewParams {
    let current = live::engaged();
    let items = ROSTER
        .iter()
        .enumerate()
        .map(|(recipe, entry)| SelectionItem {
            name: entry.name.to_owned(),
            description: Some(entry.blurb.to_owned()),
            is_current: current == Some(recipe),
            dismiss_on_select: true,
            actions: vec![Box::new(move |_: &AppEventSender| live::engage(recipe))],
            ..Default::default()
        })
        .collect();
    let params = SelectionViewParams {
        title: Some("Shimmer".to_owned()),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        initial_selected_idx: current,
        max_visible_rows: ROSTER.len(),
        ..SelectionViewParams::picker()
    };
    let Some(palette) = live::palette() else {
        return SelectionViewParams {
            subtitle: Some(
                "Shimmers need 24-bit color; this terminal keeps the classic sweep.".to_owned(),
            ),
            ..params
        };
    };
    let preview = Arc::new(Mutex::new(Preview::new(
        current.unwrap_or_default(),
        palette,
    )));
    let pane = |preview: &Arc<Mutex<Preview>>| PreviewPane {
        preview: Arc::clone(preview),
        frames: frames.clone(),
        motion,
    };
    let follow = Arc::clone(&preview);
    SelectionViewParams {
        subtitle: Some("Move to preview; Enter swaps the live rows.".to_owned()),
        side_content: Box::new(pane(&preview)),
        side_content_width: SideContentWidth::Half,
        side_content_min_width: PREVIEW_WIDTH,
        stacked_side_content: Some(Box::new(pane(&preview))),
        preserve_side_content_bg: true,
        on_selection_changed: Some(Box::new(move |recipe, _: &AppEventSender| {
            follow
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .show(recipe);
        })),
        ..params
    }
}

/// The highlighted shimmer playing the script from its start.
struct Preview {
    engine: Engine,
    palette: Palette,
    script: Script,
    stepped: Instant,
    ticks: u64,
}

impl Preview {
    fn new(recipe: usize, palette: Palette) -> Self {
        Self {
            engine: Engine::new(recipe, palette),
            palette,
            script: Script::new(),
            stepped: Instant::now(),
            ticks: 0,
        }
    }

    fn show(&mut self, recipe: usize) {
        *self = Self::new(recipe, self.palette);
    }
}

struct PreviewPane {
    preview: Arc<Mutex<Preview>>,
    frames: FrameRequester,
    motion: MotionMode,
}

impl Renderable for PreviewPane {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height <= PREVIEW_ROWS {
            return;
        }
        let animated = self.motion == MotionMode::Animated
            && self.frames.mode() == crate::tui::PresentationMode::Normal;
        let mut preview = self.preview.lock().unwrap_or_else(PoisonError::into_inner);
        let Preview {
            engine,
            palette,
            script,
            stepped,
            ticks,
        } = &mut *preview;
        if animated {
            let now = Instant::now();
            let tick = Duration::from_secs_f64(TICK);
            // Hidden or suspended previews pause; never replay an unbounded wall-clock backlog.
            if now.saturating_duration_since(*stepped) > Duration::from_secs(1) {
                *stepped = now;
            }
            while now.saturating_duration_since(*stepped) >= tick {
                *stepped += tick;
                *ticks += 1;
                feed(engine, script, *ticks);
            }
        }
        let area = Rect {
            y: area.y.saturating_add(1),
            height: area.height.saturating_sub(1),
            ..area
        };
        stage_rows(engine, script, *ticks, *palette, area, buf);
        if animated {
            self.frames.schedule_frame_in(Duration::from_secs_f64(TICK));
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        PREVIEW_ROWS + 1
    }
}

/// Steps `engine` through the script's `tick`th tick, counting from one.
pub(super) fn feed(engine: &mut Engine, script: &Script, tick: u64) {
    let t = tick as f64 * TICK;
    let (spell, _, tokens) = script.at(t);
    let (_, _, before) = script.at(t - TICK);
    engine.step(u32::try_from(tokens - before).unwrap_or(u32::MAX), spell);
}

/// Draws the live rows as codex-tui lays them out, with the script `tick` ticks in: a running tool
/// and its output on the first two rows of `area`, the status row on the fourth.
pub(super) fn stage_rows(
    engine: &mut Engine,
    script: &Script,
    tick: u64,
    palette: Palette,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.height < PREVIEW_ROWS {
        return;
    }
    let t = tick as f64 * TICK;
    let (_, header, _) = script.at(t);
    let timer = 63 + (t as u64) % 480;
    let tool = Line::from(vec![
        engine.tool(palette),
        " ".into(),
        "Command".bold(),
        " cargo test -p codex-tui --lib".into(),
    ]);
    buf.set_line(area.x, area.y, &tool, area.width);
    let output = Line::from("    Compiling codex-tui v0.0.0".dim());
    buf.set_line(area.x, area.y + 1, &output, area.width);
    let (mut lead, backdrop) = engine.status(
        header, timer, palette, /*mark*/ true, /*letters*/ true,
    );
    lead.extend([
        Span::from(" "),
        format!("({} • ⌃c to interrupt)", fmt_elapsed_compact(timer)).dim(),
    ]);
    let y = area.y + 3;
    buf.set_line(area.x, y, &Line::from(lead), area.width);
    for (x, bg) in (area.x..area.right()).zip(&backdrop) {
        buf[(x, y)].set_bg(color(*bg));
    }
}
