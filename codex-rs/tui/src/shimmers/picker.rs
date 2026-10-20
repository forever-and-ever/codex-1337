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
use super::stage::Rgb;
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
pub(super) const PREVIEW_ROWS: u16 = 4;

pub(crate) fn picker(frames: FrameRequester, motion: MotionMode) -> SelectionViewParams {
    let current = live::engaged();
    let items = ROSTER
        .iter()
        .enumerate()
        .map(|(recipe, entry)| SelectionItem {
            name: entry.recipe.name.to_owned(),
            description: Some(format!("{} — {}", entry.recipe.blurb, entry.credit)),
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
    let pane = |preview: &Arc<Mutex<Preview>>, frames| PreviewPane {
        preview: Arc::clone(preview),
        frames,
        motion,
    };
    let follow = Arc::clone(&preview);
    SelectionViewParams {
        subtitle: Some("Move to preview; Enter swaps the live rows.".to_owned()),
        side_content: Box::new(pane(&preview, frames.clone())),
        side_content_width: SideContentWidth::Half,
        side_content_min_width: PREVIEW_WIDTH,
        stacked_side_content: Some(Box::new(pane(&preview, frames))),
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
        stage_rows(engine, Scene::scripted(script, *ticks), *palette, area, buf);
        if animated {
            self.frames.schedule_frame_in(Duration::from_secs_f64(TICK));
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        PREVIEW_ROWS + 1
    }
}

/// Steps `engine` through the script's `tick`th tick, counting from one.
#[expect(
    clippy::cast_precision_loss,
    reason = "Preview ticks become approximate animation seconds, not semantic time or token accounting."
)]
pub(super) fn feed(engine: &mut Engine, script: &Script, tick: u64) {
    let t = tick as f64 * TICK;
    let (spell, _, tokens) = script.at(t);
    let (_, _, before) = script.at(t - TICK);
    engine.step(u32::try_from(tokens - before).unwrap_or(u32::MAX), spell);
}

/// What the live rows show around a shimmer: a running tool's command and output, and the status
/// row's header and timer.
#[derive(Clone, Copy)]
pub(super) struct Scene<'a> {
    pub(super) command: &'a str,
    pub(super) output: &'a str,
    pub(super) header: &'a str,
    pub(super) timer: u64,
}

impl Scene<'static> {
    /// The preview's scene `tick` ticks into `script`.
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Preview time is a nonnegative visual clock; the displayed timer deliberately uses whole seconds modulo 480."
    )]
    fn scripted(script: &Script, tick: u64) -> Self {
        let t = tick as f64 * TICK;
        Self {
            command: "cargo test -p codex-tui --lib",
            output: "Compiling codex-tui v0.0.0",
            header: script.at(t).1,
            timer: 63 + (t as u64) % 480,
        }
    }
}

/// Draws the live rows as codex-tui lays them out: `scene`'s running tool and its output on the
/// first two rows of `area`, the status row on the fourth.
pub(super) fn stage_rows(
    engine: &mut Engine,
    scene: Scene<'_>,
    palette: Palette,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.height < PREVIEW_ROWS {
        return;
    }
    let Scene {
        command,
        output,
        header,
        timer,
    } = scene;
    let tool = Line::from(vec![
        engine.tool(palette),
        " ".into(),
        "Command".bold(),
        format!(" {command}").into(),
    ]);
    buf.set_line(area.x, area.y, &tool, area.width);
    let output = Line::from(format!("    {output}").dim());
    buf.set_line(area.x, area.y + 1, &output, area.width);
    let (mut lead, backdrop) = engine.status(
        header, timer, palette, /*mark*/ true, /*letters*/ true,
    );
    lead.extend([
        Span::from(" "),
        format!("({} • ⌃c to interrupt)", fmt_elapsed_compact(timer)).dim(),
    ]);
    lay_status(lead, &backdrop, area, area.y + 3, buf);
}

/// Lays a status row's `lead` on row `y` of `area`, over the shimmer's `backdrop`.
pub(super) fn lay_status(
    lead: Vec<Span<'static>>,
    backdrop: &[Rgb],
    area: Rect,
    y: u16,
    buf: &mut Buffer,
) {
    buf.set_line(area.x, y, &Line::from(lead), area.width);
    for (x, bg) in (area.x..area.right()).zip(backdrop) {
        buf[(x, y)].set_bg(color(*bg));
    }
}
