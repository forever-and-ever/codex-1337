//! The upstream inline renderer's plan framing.

use super::HyperlinkLine;
use super::Line;
use super::Stylize;
use super::prefix_proposed_plan_lines;
use super::proposed_plan_style;

pub(crate) fn render_plan_rows(
    lines: Vec<HyperlinkLine>,
    first: bool,
    top: bool,
    bottom: bool,
) -> Vec<HyperlinkLine> {
    let mut out = Vec::new();
    if first {
        out.push(HyperlinkLine::new(
            vec!["• ".dim(), "Proposed Plan".bold()].into(),
        ));
        out.push(HyperlinkLine::new(Line::from(" ")));
    }
    let mut plan = Vec::new();
    if top {
        plan.push(HyperlinkLine::new(Line::from(" ")));
    }
    plan.extend(lines);
    if bottom {
        plan.push(HyperlinkLine::new(Line::from(" ")));
    }
    out.extend(
        prefix_proposed_plan_lines(plan)
            .into_iter()
            .map(|line| line.style(proposed_plan_style())),
    );
    out
}
