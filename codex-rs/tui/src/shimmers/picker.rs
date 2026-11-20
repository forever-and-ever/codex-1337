//! `/shimmer`: the stock roster beside a live preview of the highlighted shimmer; `r` engages a
//! random other one.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Instant;

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use libshimmer::Motion;
use libshimmer::Palette;
use libshimmer::Recipe;
use libshimmer::Row;
use libshimmer::STALE;
use libshimmer::Shimmer;
use libshimmer::TICK;
use libshimmer::script;
use libshimmer::stock::ROSTER;
use libshimmer_ratatui::InkExt as _;
use libshimmer_ratatui::RowExt as _;

use super::live;
use super::live::GLYPHS;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionRowDisplay;
use crate::bottom_pane::SelectionSecondaryAction;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::SideContentWidth;
use crate::key_hint;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;
use crate::motion::MotionMode;
use crate::render::renderable::Renderable;
use crate::status_indicator_widget::fmt_elapsed_compact;
use crate::tui::FrameRequester;

/// Columns the preview needs beside the list.
const PREVIEW_WIDTH: u16 = 44;
/// Rows of the preview: a running tool and its output, a blank row, the status row.
pub(super) const PREVIEW_ROWS: u16 = 4;

pub(crate) fn picker(
    frames: FrameRequester,
    motion: MotionMode,
    keymap: &ListKeymap,
) -> SelectionViewParams {
    let recipes: Vec<_> = ROSTER
        .iter()
        .filter(|recipe| live::offered(recipe))
        .collect();
    let current = live::engaged().and_then(|engaged| {
        recipes
            .iter()
            .position(|recipe| std::ptr::eq(*recipe, engaged))
    });
    let roll = key_hint::plain(KeyCode::Char('r'));
    let mut hint = Vec::new();
    if let Some(accept) = keymap.primary_hint(ListAction::Accept) {
        hint.extend([accept.into(), " select · ".dim()]);
    }
    hint.extend([roll.into(), " random".dim()]);
    if let Some(cancel) = keymap.primary_hint(ListAction::Cancel) {
        hint.extend([" · ".dim(), cancel.into(), " back".dim()]);
    }
    let hint = Line::from(hint);
    let items: Vec<SelectionItem> = recipes
        .iter()
        .copied()
        .enumerate()
        .map(|(index, recipe)| SelectionItem {
            name: recipe.name().to_owned(),
            description: Some(recipe.blurb().to_owned()),
            is_current: current == Some(index),
            dismiss_on_select: true,
            actions: vec![Box::new(move |_: &AppEventSender| live::engage(recipe))],
            secondary_action: Some(SelectionSecondaryAction {
                key: roll,
                action: Box::new(|_: &AppEventSender| {
                    tokio::spawn(live::engage_random());
                }),
                footer_hint: hint.clone(),
            }),
            ..Default::default()
        })
        .collect();
    let widest = widest_row(&items);
    let params = SelectionViewParams {
        title: Some("Shimmer".to_owned()),
        items,
        initial_selected_idx: current,
        max_visible_rows: recipes.len(),
        row_display: SelectionRowDisplay::SingleLine,
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
        recipes[current.unwrap_or_default()],
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
        // `Half` never gives the list less than the preview, so a preview no narrower than the
        // widest row keeps every blurb whole; a narrower terminal stacks the preview below.
        side_content_min_width: PREVIEW_WIDTH.max(widest),
        stacked_side_content: Some(Box::new(pane(&preview, frames))),
        preserve_side_content_bg: true,
        on_selection_changed: Some(Box::new(move |recipe, _: &AppEventSender| {
            follow
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .show(recipes[recipe]);
        })),
        ..params
    }
}

/// Columns the widest row needs to show its description whole, as `ListSelectionView` lays out
/// single-line rows: a `› 23. ` gutter, the name and any ` (current)` marker, then a two-column
/// gap before the description column that every row shares.
fn widest_row(items: &[SelectionItem]) -> u16 {
    let names = items.iter().enumerate().map(|(index, item)| {
        let marker = if item.is_current { " (current)" } else { "" };
        format!("› {}. {}{marker}", index + 1, item.name).width()
    });
    let descriptions = items
        .iter()
        .filter_map(|item| item.description.as_deref())
        .map(UnicodeWidthStr::width);
    let columns = names.max().unwrap_or_default() + 2 + descriptions.max().unwrap_or_default();
    u16::try_from(columns).unwrap_or(u16::MAX)
}

/// The highlighted shimmer playing the script from its start.
struct Preview {
    shimmer: Shimmer,
    palette: Palette,
    stepped: Instant,
    ticks: u32,
}

impl Preview {
    #[expect(
        clippy::expect_used,
        reason = "The picker offers only recipes that fit codex-tui's wide captions and octants."
    )]
    fn new(recipe: &'static Recipe, palette: Palette) -> Self {
        Self {
            shimmer: Shimmer::new(recipe, rand::random(), GLYPHS)
                .expect("the preview recipe fits codex-tui"),
            palette,
            stepped: Instant::now(),
            ticks: 0,
        }
    }

    fn show(&mut self, recipe: &'static Recipe) {
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
            shimmer,
            palette,
            stepped,
            ticks,
        } = &mut *preview;
        if animated {
            let now = Instant::now();
            let tick = TICK;
            // Hidden or suspended previews pause; never replay an unbounded wall-clock backlog.
            if now.saturating_duration_since(*stepped) > STALE {
                *stepped = now;
            }
            while now.saturating_duration_since(*stepped) >= tick {
                *stepped += tick;
                *ticks += 1;
                feed(shimmer, *ticks);
            }
        }
        let credit = Line::from(format!("by {}", shimmer.recipe().credit())).dim();
        buf.set_line(area.x, area.y, &credit, area.width);
        let area = Rect {
            y: area.y.saturating_add(1),
            height: area.height.saturating_sub(1),
            ..area
        };
        stage_rows(shimmer, Scene::scripted(*ticks), *palette, area, buf);
        if animated {
            self.frames.schedule_frame_in(TICK);
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        PREVIEW_ROWS + 1
    }
}

/// Advances `shimmer` through the script's `tick`th tick, counting from one, hearing its song if
/// the shimmer listens.
pub(super) fn feed(shimmer: &mut Shimmer, tick: u32) {
    let cue = script::cue(TICK * tick);
    shimmer.set_spell(cue.spell);
    shimmer.show(cue.caption);
    shimmer.set_intensity(cue.intensity);
    if shimmer.recipe().listens() {
        shimmer.hear(&script::sound(TICK * tick.saturating_sub(1), TICK * tick));
    }
    shimmer.advance(TICK);
}

/// What the live rows show around a shimmer: a running tool's command and output, and the status
/// row's caption and timer.
#[derive(Clone, Copy)]
pub(super) struct Scene<'a> {
    pub(super) command: &'a str,
    pub(super) output: &'a str,
    pub(super) caption: &'a str,
    pub(super) timer: u64,
}

impl Scene<'static> {
    /// The preview's scene after `tick` ticks.
    fn scripted(tick: u32) -> Self {
        let at = TICK * tick;
        Self {
            command: "cargo test -p codex-tui --lib",
            output: "Compiling codex-tui v0.0.0",
            caption: script::cue(at).caption,
            timer: 63 + at.as_secs() % 480,
        }
    }
}

/// Draws the live rows as codex-tui lays them out: `scene`'s running tool and its output on the
/// first two rows of `area`, the status row on the fourth.
pub(super) fn stage_rows(
    shimmer: &mut Shimmer,
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
        caption,
        timer,
    } = scene;
    let tool = Line::from(vec![
        shimmer.bullet(palette).span("•"),
        " ".into(),
        "Command".bold(),
        format!(" {command}").into(),
    ]);
    buf.set_line(area.x, area.y, &tool, area.width);
    let output = Line::from(format!("    {output}").dim());
    buf.set_line(area.x, area.y + 1, &output, area.width);
    shimmer.show(caption);
    let row = shimmer.status(
        palette,
        Motion {
            mark: true,
            letters: true,
        },
    );
    let mut lead = row.spans();
    lead.extend([
        Span::from(" "),
        format!("({} • ⌃c to interrupt)", fmt_elapsed_compact(timer)).dim(),
    ]);
    lay_status(lead, &row, area, area.y + 3, buf);
}

/// Lays a status row's `lead` on row `y` of `area`, over the shimmer's `backdrop`.
pub(super) fn lay_status(
    lead: Vec<Span<'static>>,
    row: &Row,
    area: Rect,
    y: u16,
    buf: &mut Buffer,
) {
    buf.set_line(area.x, y, &Line::from(lead), area.width);
    row.shade(Rect { y, ..area }, buf);
}
