//! Live captures: a frame as the widgets painted it, before warpaint binds it, named in the
//! vocabulary `scripts/warpaint` renders. The binder's own resolution names each cell, and the
//! real binder proves every name against the pack worn.

use std::cell::RefCell;
use std::io;
use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::buffer::CellDiffOption;
use ratatui::buffer::CellWidth;
use ratatui::style::Color;
use ratatui::style::Modifier;

use super::bind::Binder;
use super::bind::Canvas;
use super::bind::Depth;
use super::bind::Source;
use super::pack::Pack;
use super::paint::Figure;
use super::paint::Ground;
use super::paint::Paint;
use crate::terminal_hyperlinks::strip_osc8;
use crate::terminal_palette::indexed_color;
use crate::terminal_palette::rgb_color;

/// Modifiers a capture names; no other may reach a captured frame.
const MODS: [(Modifier, &str); 6] = [
    (Modifier::BOLD, "bold"),
    (Modifier::DIM, "dim"),
    (Modifier::ITALIC, "italic"),
    (Modifier::UNDERLINED, "underline"),
    (Modifier::REVERSED, "reverse"),
    (Modifier::CROSSED_OUT, "strike"),
];

enum Witness {
    Disarmed,
    Armed,
    Saw(Buffer),
}

thread_local! {
    static WITNESS: RefCell<Witness> = const { RefCell::new(Witness::Disarmed) };
}

/// Keeps the frame about to be bound, if a capture awaits one.
pub(super) fn witness(frame: &Buffer) {
    WITNESS.with_borrow_mut(|witness| {
        if !matches!(witness, Witness::Disarmed) {
            *witness = Witness::Saw(frame.clone());
        }
    });
}

/// Runs `f` as on a truecolor terminal invested with the pack worn, where upstream reads the
/// pack's ink and canvas as the terminal's defaults.
pub(crate) fn invested<R>(f: impl FnOnce() -> R) -> R {
    let colors = super::with_warpaint(|warpaint| {
        let pack = warpaint.pack();
        crate::terminal_probe::DefaultColors {
            fg: pack.figure[Figure::Ink].tuple(),
            bg: pack.ground[Ground::Canvas].tuple(),
        }
    });
    crate::terminal_palette::with_test_default_colors(colors, f)
}

/// Runs `draw`, returning its result and the last frame it flushed, unbound.
pub(crate) fn unbound<R>(draw: impl FnOnce() -> R) -> (R, Buffer) {
    WITNESS.set(Witness::Armed);
    let drawn = draw();
    match WITNESS.replace(Witness::Disarmed) {
        Witness::Saw(frame) => (drawn, frame),
        Witness::Armed | Witness::Disarmed => panic!("`draw` flushed no frame"),
    }
}

/// Writes `frame` as `scripts/warpaint/captures/<name>.json`: `{name, width, height, rows}`,
/// each row the runs of columns that share colors and modifiers, as the terminal shows them.
pub(crate) fn write(name: &str, frame: &Buffer) -> io::Result<()> {
    let pack = super::with_warpaint(super::Warpaint::pack);
    let mut binder = Binder::new(pack, Depth::TrueColor, Canvas::Painted);
    let area = frame.area;
    let rows: Vec<String> = (area.top()..area.bottom())
        .map(|y| {
            let runs: Vec<String> = runs(frame, y, pack, &mut binder)
                .into_iter()
                .map(|(text, [fg, bg, mods])| {
                    format!(
                        r#"      {{"text": {}, "fg": "{fg}", "bg": "{bg}", "mods": [{mods}]}}"#,
                        quoted(&text)
                    )
                })
                .collect();
            format!("    [\n{}\n    ]", runs.join(",\n"))
        })
        .collect();
    let json = format!(
        "{{\n  \"name\": {},\n  \"width\": {},\n  \"height\": {},\n  \"rows\": [\n{}\n  ]\n}}\n",
        quoted(name),
        area.width,
        area.height,
        rows.join(",\n")
    );
    let captures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/warpaint/captures");
    std::fs::write(captures.join(format!("{name}.json")), json)
}

/// Row `y`'s runs, named and each cell's names proven against `binder`: text, then foreground,
/// background and modifiers.
fn runs(frame: &Buffer, y: u16, pack: &Pack, binder: &mut Binder) -> Vec<(String, [String; 3])> {
    let mut runs: Vec<(String, [String; 3])> = Vec::new();
    let mut covered = 0;
    for x in frame.area.left()..frame.area.right() {
        if covered > 0 {
            covered -= 1;
            continue;
        }
        let cell = &frame[(x, y)];
        let (fg, bg, modifier) = Source::of_cell(cell.fg, cell.bg, cell.modifier);
        assert!(
            binder.bind(cell.fg, cell.bg, cell.modifier)
                == (shown(pack, fg, false), shown(pack, bg, true), modifier),
            "the binder disagrees with {} on {} at ({x}, {y})",
            name_of(fg),
            name_of(bg)
        );
        let width = cell.cell_width();
        let text = if cell.diff_option == CellDiffOption::Skip || width == 0 {
            " ".to_string()
        } else {
            covered = width - 1;
            strip_osc8(cell.symbol())
        };
        let style = [name_of(fg), name_of(bg), mods(modifier)];
        match runs.last_mut() {
            Some((run, shared)) if *shared == style => run.push_str(&text),
            _ => runs.push((text, style)),
        }
    }
    runs
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).expect("strings serialize")
}

/// A source as `scripts/warpaint/packs.py` resolves it.
fn name_of(source: Source) -> String {
    match source {
        Source::Reset => "reset".to_string(),
        Source::Paint(Paint::Ground(ground)) => format!("ground:{}", ground.key()),
        Source::Paint(Paint::Figure(figure)) => format!("figure:{}", figure.key()),
        Source::Paint(Paint::Syntax(syntax)) => format!("syntax:{}", syntax.key()),
        Source::Paint(Paint::Heat(step)) => format!("heat:{step}"),
        Source::Paint(Paint::Livery(slot)) => format!("livery:{slot}"),
        Source::Terminal(index) => format!("terminal:{index}"),
        Source::Xterm(index) => format!("xterm:{index}"),
        Source::Rgb(r, g, b) => format!("rgb:#{r:02x}{g:02x}{b:02x}"),
    }
}

fn mods(modifier: Modifier) -> String {
    let named = MODS
        .iter()
        .fold(Modifier::empty(), |named, &(each, _)| named | each);
    assert!(
        named.contains(modifier),
        "a capture cannot name {:?}",
        modifier.difference(named)
    );
    MODS.iter()
        .filter(|&&(each, _)| modifier.contains(each))
        .map(|(_, name)| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the binder shows a source as, in truecolor on a painted canvas.
fn shown(pack: &Pack, source: Source, background: bool) -> Color {
    let rgb = match source {
        Source::Reset if background => pack.ground[Ground::Canvas],
        Source::Reset => pack.figure[Figure::Ink],
        Source::Paint(paint) => pack.rgb(paint),
        Source::Terminal(index) => pack.terminal[usize::from(index)],
        Source::Xterm(index) => return indexed_color(index),
        Source::Rgb(r, g, b) => return rgb_color((r, g, b)),
    };
    rgb_color(rgb.tuple())
}
