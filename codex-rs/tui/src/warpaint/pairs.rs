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

use super::paint::Figure;
use super::paint::Ground;
use super::paint::HEAT_STEPS;
use super::paint::LIVERY_SLOTS;
use super::paint::Syntax;
use super::scopes;

/// Where prose lands: the page, panels, every band, and a selected row.
const SURFACES: &[Ground] = &[
    Ground::Canvas,
    Ground::Trough,
    Ground::Dais,
    Ground::Selection,
    Ground::Prompt,
    Ground::Response,
    Ground::Reasoning,
    Ground::Aside,
    Ground::Subagent,
];
/// Where ink lands: prose's grounds and diff lines.
const TEXT: &[Ground] = &[
    Ground::Canvas,
    Ground::Trough,
    Ground::Dais,
    Ground::Selection,
    Ground::Prompt,
    Ground::Response,
    Ground::Reasoning,
    Ground::Aside,
    Ground::Subagent,
    Ground::Added,
    Ground::Removed,
];
/// Inline code: prose's grounds but a selected row.
const INLINE: &[Ground] = &[
    Ground::Canvas,
    Ground::Trough,
    Ground::Dais,
    Ground::Prompt,
    Ground::Response,
    Ground::Reasoning,
    Ground::Aside,
    Ground::Subagent,
];
/// Where code is highlighted: blocks on the page, in panels, in the agents' bands and the
/// reasoning they disclose, and diffs.
const CODE: &[Ground] = &[
    Ground::Canvas,
    Ground::Trough,
    Ground::Response,
    Ground::Reasoning,
    Ground::Subagent,
    Ground::Added,
    Ground::Removed,
];
/// Thread livery marks rows on the page, in panels and on raised blocks.
const LIVERY: &[Ground] = &[Ground::Canvas, Ground::Trough, Ground::Dais];
/// The activity heat map is drawn on the page.
const HEAT: &[Ground] = &[Ground::Canvas];
/// Tool output sits on the page or in the band of the turn that ran it.
const TERMINAL: &[Ground] = &[
    Ground::Canvas,
    Ground::Prompt,
    Ground::Response,
    Ground::Aside,
    Ground::Subagent,
];

/// Every ground a call site paints `figure` on.
fn painted(figure: Figure) -> &'static [Ground] {
    match figure {
        Figure::Ink | Figure::InkMuted | Figure::InkFaint => TEXT,
        Figure::InkSelected => &[Ground::Selection],
        Figure::Rule => &[
            Ground::Canvas,
            Ground::Dais,
            Ground::Response,
            Ground::Reasoning,
            Ground::Subagent,
        ],
        Figure::Code => INLINE,
        Figure::Accent
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
    let ramp = |on: &[Ground]| grounds(|ground| on.contains(ground));
    json!({
        "ground": keys(Ground::ALL.iter().map(|member| member.key())),
        "figure": keys(Figure::ALL.iter().map(|member| member.key())),
        "syntax": keys(Syntax::ALL.iter().map(|member| member.key())),
        "ramps": {
            "livery": {"min": 1, "max": LIVERY_SLOTS},
            "heat": {"len": HEAT_STEPS},
            "terminal": {"len": 16},
        },
        "pairs": {
            "figure": figure,
            "syntax": syntax,
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
