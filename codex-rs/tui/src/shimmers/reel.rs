//! Reel clips: stock shimmers replaying the tick streams `scripts/shimmer-reel` cues, exported as the
//! cells codex-tui draws for them. The reel runs this test through `just shimmer-reel`.

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::buffer::Cell;
use ratatui::layout::Rect;
use ratatui::style::Color;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use super::ROSTER;
use super::RosterEntry;
use super::engine::Engine;
use super::kit::OCTANTS;
use super::picker::PREVIEW_ROWS;
use super::picker::Scene;
use super::picker::lay_status;
use super::picker::stage_rows;
use super::stage::Palette;
use super::stage::Spell;

/// The cue sheet the reel writes to `$SHIMMER_REEL/cues.json`.
#[derive(Deserialize)]
struct Cues {
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
    /// The status row's lead alone: its mark, header and tail over the backdrop.
    Status,
}

/// One tick: the paced tokens that arrived in it, its spell, the status header and the timer's
/// whole seconds.
#[derive(Deserialize)]
struct Tick(u32, Spell, String, u64);

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
    let palette = Palette {
        fg: cues.fg.into(),
        bg: cues.bg.into(),
    };
    let clips: Value = cues
        .clips
        .iter()
        .map(|cue| film(cue, palette, cues.width))
        .collect();
    let octants: String = OCTANTS.iter().collect();
    std::fs::write(
        dir.join("clips.json"),
        json!({ "octants": octants, "clips": clips }).to_string(),
    )
    .expect("the clips are written");
}

/// `cue`'s frames, one per tick, beside its shimmer's roster entry.
fn film(cue: &Cue, palette: Palette, width: u16) -> Value {
    let recipe = ROSTER
        .iter()
        .position(|entry| entry.recipe.name == cue.shimmer)
        .unwrap_or_else(|| panic!("no stock shimmer is named {:?}", cue.shimmer));
    let mut engine = Engine::new(recipe, palette);
    let height = match cue.rows {
        Rows::Live { .. } => PREVIEW_ROWS,
        Rows::Status => 1,
    };
    let area = Rect::new(0, 0, width, height);
    let frames: Value = cue
        .ticks
        .iter()
        .map(|Tick(tokens, spell, header, timer)| {
            engine.step(*tokens, *spell);
            let mut buf = Buffer::empty(area);
            match &cue.rows {
                Rows::Live { command, output } => {
                    let scene = Scene {
                        command,
                        output,
                        header,
                        timer: *timer,
                    };
                    stage_rows(&mut engine, scene, palette, area, &mut buf);
                }
                Rows::Status => {
                    let (lead, backdrop) = engine.status(
                        header, *timer, palette, /*mark*/ true, /*letters*/ true,
                    );
                    lay_status(lead, &backdrop, area, area.y, &mut buf);
                }
            }
            cells(&buf)
        })
        .collect();
    let RosterEntry { recipe, credit } = &ROSTER[recipe];
    json!({ "shimmer": recipe.name, "blurb": recipe.blurb, "credit": credit, "frames": frames })
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
