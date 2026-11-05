//! Reel clips: stock shimmers replaying the tick streams `scripts/shimmer-reel` cues, exported as the
//! cells codex-tui draws for them. The reel runs this test through `just shimmer-reel`.

use std::path::PathBuf;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::buffer::Cell;
use ratatui::layout::Rect;
use ratatui::style::Color;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use libshimmer::Glyphs;
use libshimmer::Intensity;
use libshimmer::Motion;
use libshimmer::Palette;
use libshimmer::Shimmer;
use libshimmer::Spell;
use libshimmer::TICK;
use libshimmer::script;
use libshimmer::stock::ROSTER;
use libshimmer_ratatui::RowExt as _;

use super::live::BRISK;
use super::live::GLYPHS;
use super::picker::PREVIEW_ROWS;
use super::picker::Scene;
use super::picker::lay_status;
use super::picker::stage_rows;

/// The cue sheet the reel writes to `$SHIMMER_REEL/cues.json`.
#[derive(Deserialize)]
struct Cues {
    tick: f64,
    fg: (u8, u8, u8),
    bg: (u8, u8, u8),
    width: u16,
    clips: Vec<Cue>,
}

#[derive(Deserialize)]
struct Cue {
    shimmer: String,
    rows: Rows,
    ticks: Vec<Tick>,
}

/// The rows a clip draws.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Rows {
    /// The live rows `/shimmer` previews, around a running `command` and its `output`.
    Live { command: String, output: String },
    /// The status row's lead alone: its mark, caption and tail over the backdrop.
    Status,
}

/// One tick's raw inflow rate in tokens a second, spell, caption and timer's whole seconds.
#[derive(Deserialize)]
struct Tick(f64, CueSpell, String, u64);

/// Cue sheets name only the two spells within a bout.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CueSpell {
    Busy,
    Wait,
}

impl From<CueSpell> for Spell {
    fn from(spell: CueSpell) -> Self {
        match spell {
            CueSpell::Busy => Self::Busy,
            CueSpell::Wait => Self::Wait,
        }
    }
}

#[test]
#[ignore = "exports clips for scripts/shimmer-reel, which runs it through `just shimmer-reel`"]
fn export() {
    let dir = PathBuf::from(
        std::env::var_os("SHIMMER_REEL").expect("SHIMMER_REEL names the reel's working directory"),
    );
    let cues: Cues = serde_json::from_slice(
        &std::fs::read(dir.join("cues.json")).expect("the reel wrote a cue sheet"),
    )
    .expect("the cue sheet parses");
    assert_eq!(
        Duration::from_secs_f64(cues.tick),
        TICK,
        "the cue sheet uses libshimmer ticks"
    );
    let palette = Palette {
        fg: cues.fg.into(),
        bg: cues.bg.into(),
    };
    let clips: Value = cues
        .clips
        .iter()
        .map(|cue| film(cue, palette, cues.width))
        .collect();
    let octants: String = (0..=u8::MAX).map(Glyphs::octant).collect();
    let roster: Value = ROSTER
        .iter()
        .map(|recipe| {
            json!({
                "name": recipe.name(), "blurb": recipe.blurb(), "credit": recipe.credit(),
            })
        })
        .collect();
    std::fs::write(
        dir.join("clips.json"),
        json!({ "octants": octants, "clips": clips, "roster": roster }).to_string(),
    )
    .expect("the clips are written");
}

/// `cue`'s frames, one per tick, beside its shimmer's roster entry.
fn film(cue: &Cue, palette: Palette, width: u16) -> Value {
    let recipe = ROSTER
        .iter()
        .find(|recipe| recipe.name() == cue.shimmer)
        .unwrap_or_else(|| panic!("no stock shimmer is named {:?}", cue.shimmer));
    let mut shimmer: Shimmer = Shimmer::new(recipe, 7, GLYPHS).unwrap();
    let height = match cue.rows {
        Rows::Live { .. } => PREVIEW_ROWS,
        Rows::Status => 1,
    };
    let area = Rect::new(0, 0, width, height);
    let frames: Value = (0_u32..)
        .zip(&cue.ticks)
        .map(|(n, Tick(rate, spell, caption, timer))| {
            shimmer.set_spell((*spell).into());
            shimmer.show(caption);
            shimmer.set_intensity(Intensity::new(*rate / BRISK));
            // A shimmer that listens dances to the script's song.
            if recipe.listens() {
                shimmer.hear(&script::sound(TICK * n, TICK * (n + 1)));
            }
            shimmer.advance(TICK);
            let mut buf = Buffer::empty(area);
            match &cue.rows {
                Rows::Live { command, output } => {
                    let scene = Scene {
                        command,
                        output,
                        caption,
                        timer: *timer,
                    };
                    stage_rows(&mut shimmer, scene, palette, area, &mut buf);
                }
                Rows::Status => {
                    let row = shimmer.status(
                        palette,
                        Motion {
                            mark: true,
                            letters: true,
                        },
                    );
                    lay_status(row.spans(), &row, area, area.y, &mut buf);
                }
            }
            cells(&buf)
        })
        .collect();
    json!({ "shimmer": recipe.name(), "blurb": recipe.blurb(), "credit": recipe.credit(), "frames": frames })
}

/// `buf`'s rows of `[symbol, fg, bg, modifier bits, underline]` cells, colors as `0xRRGGBB` or null
/// for the terminal's default, each row without its trailing blank cells.
fn cells(buf: &Buffer) -> Value {
    buf.content
        .chunks(usize::from(buf.area.width))
        .map(|row| {
            let used = row
                .iter()
                .rposition(|cell| *cell != Cell::EMPTY)
                .map_or(0, |last| last + 1);
            row[..used]
                .iter()
                .map(|cell| {
                    json!([
                        cell.symbol(),
                        rgb(cell.fg),
                        rgb(cell.bg),
                        cell.modifier.bits(),
                        rgb(cell.underline_color),
                    ])
                })
                .collect::<Value>()
        })
        .collect()
}

fn rgb(color: Color) -> Value {
    match color {
        Color::Reset => Value::Null,
        Color::Rgb(r, g, b) => json!(u32::from_be_bytes([0, r, g, b])),
        other => panic!("the live rows paint RGB or the default, never {other:?}"),
    }
}
