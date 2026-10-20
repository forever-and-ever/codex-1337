//! The pairs warpaint paints: each foreground with every ground it lands on. The generator solves
//! and proves exactly these pairs, read from `scripts/warpaint/roles.json`, which the test below
//! keeps true to this file; regenerate it with
//! `WARPAINT_WRITE_ROLES=1 just test -p codex-tui --lib roles_json_matches_the_declaration`.
//!
//! A call site that paints a figure on a new ground extends its list first. Whatever the scope
//! theme paints lands wherever code is highlighted, so its figures are declared over code's
//! grounds as well as their own.

use pretty_assertions::assert_eq;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use super::paint::Bracket;
use super::paint::Figure;
use super::paint::Ground;
use super::paint::HEAT_STEPS;
use super::paint::LIVERY_SLOTS;
use super::paint::Syntax;
use super::scopes;

/// Where prose lands: the page, raised blocks, a selected row and the prompt's panels.
const SURFACES: &[Ground] = &[
    Ground::Canvas,
    Ground::Dais,
    Ground::Selection,
    Ground::Prompt,
];
/// Where ink lands: prose's grounds and diff lines.
const TEXT: &[Ground] = &[
    Ground::Canvas,
    Ground::Dais,
    Ground::Selection,
    Ground::Prompt,
    Ground::Added,
    Ground::Removed,
];
/// Inline code: prose's grounds but a selected row.
const INLINE: &[Ground] = &[Ground::Canvas, Ground::Dais, Ground::Prompt];
/// Where code is highlighted: blocks on the page, commands in the prompt's panels, and diffs.
const CODE: &[Ground] = &[
    Ground::Canvas,
    Ground::Prompt,
    Ground::Added,
    Ground::Removed,
];
/// Thread livery marks rows on the page, on raised blocks and in the prompt's panels.
const LIVERY: &[Ground] = &[Ground::Canvas, Ground::Dais, Ground::Prompt];
/// The activity heat map is drawn on the page.
const HEAT: &[Ground] = &[Ground::Canvas];
/// Tool output sits on the page.
const TERMINAL: &[Ground] = &[Ground::Canvas];
/// Brackets hang in the page's gutter.
const BRACKETS: &[Ground] = &[Ground::Canvas];

/// Every ground a call site paints `figure` on.
fn painted(figure: Figure) -> &'static [Ground] {
    match figure {
        Figure::Ink | Figure::InkMuted | Figure::InkFaint => TEXT,
        Figure::InkSelected => &[Ground::Selection],
        Figure::Tab => &[Ground::Canvas],
        Figure::Rule => &[Ground::Canvas, Ground::Dais],
        Figure::Code => INLINE,
        Figure::Label
        | Figure::Accent
        | Figure::Link
        | Figure::Success
        | Figure::Caution
        | Figure::Failure
        | Figure::Info => SURFACES,
        Figure::Added => &[Ground::Canvas, Ground::Added],
        Figure::Removed => &[Ground::Canvas, Ground::Removed],
    }
}

fn keys(keys: impl Iterator<Item = &'static str>) -> Value {
    Value::Array(keys.map(Value::from).collect())
}

/// The grounds `lands` admits, in declaration order.
fn grounds(lands: impl Fn(&Ground) -> bool) -> Value {
    keys(
        Ground::ALL
            .iter()
            .filter(|ground| lands(ground))
            .map(|ground| ground.key()),
    )
}

/// The vocabulary as the generator reads it.
fn roles_json() -> Value {
    let highlighted = |figure: Figure| {
        scopes::RULES
            .iter()
            .any(|&(token, _)| token == figure.token())
    };
    let figure: Map<String, Value> = Figure::ALL
        .iter()
        .map(|&figure| {
            let on = grounds(|ground| {
                painted(figure).contains(ground) || highlighted(figure) && CODE.contains(ground)
            });
            (figure.key().to_owned(), on)
        })
        .collect();
    let syntax: Map<String, Value> = Syntax::ALL
        .iter()
        .map(|syntax| {
            (
                syntax.key().to_owned(),
                grounds(|ground| CODE.contains(ground)),
            )
        })
        .collect();
    let bracket: Map<String, Value> = Bracket::ALL
        .iter()
        .map(|bracket| {
            (
                bracket.key().to_owned(),
                grounds(|ground| BRACKETS.contains(ground)),
            )
        })
        .collect();
    let ramp = |on: &[Ground]| grounds(|ground| on.contains(ground));
    json!({
        "ground": keys(Ground::ALL.iter().map(|member| member.key())),
        "figure": keys(Figure::ALL.iter().map(|member| member.key())),
        "syntax": keys(Syntax::ALL.iter().map(|member| member.key())),
        "bracket": keys(Bracket::ALL.iter().map(|member| member.key())),
        "ramps": {
            "livery": {"min": 1, "max": LIVERY_SLOTS},
            "heat": {"len": HEAT_STEPS},
            "terminal": {"len": 16},
        },
        "pairs": {
            "figure": figure,
            "syntax": syntax,
            "bracket": bracket,
            "ramp": {
                "livery": ramp(LIVERY),
                "heat": ramp(HEAT),
                "terminal": ramp(TERMINAL),
            },
        },
    })
}

#[test]
fn roles_json_matches_the_declaration() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/warpaint/roles.json");
    let declared = format!(
        "{}\n",
        serde_json::to_string_pretty(&roles_json()).expect("roles.json serializes")
    );
    if std::env::var_os("WARPAINT_WRITE_ROLES").is_some() {
        std::fs::write(&path, &declared).expect("write roles.json");
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(committed, declared, "scripts/warpaint/roles.json is stale");
}
