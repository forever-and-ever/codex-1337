use super::HistoryCell;
use super::PreambleCell;
use crate::terminal_hyperlinks::visible_lines;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::PreambleChange;
use codex_protocol::PreamblePart;
use codex_protocol::PreambleSection;

fn part(section: PreambleSection, group: Option<&str>, label: &str, tokens: u64) -> PreamblePart {
    PreamblePart {
        section,
        group: group.map(str::to_string),
        label: label.to_string(),
        tokens,
    }
}

#[test]
fn preamble_discloses_parts_largest_first() {
    use PreambleSection::Context;
    use PreambleSection::Instructions;
    use PreambleSection::Tools;
    let cell = PreambleCell::from_item(ThreadItem::Preamble {
        id: "preamble:turn:response".to_string(),
        response_id: "response".to_string(),
        completed_at_ms: 0,
        parts: vec![
            part(
                Instructions,
                /*group*/ None,
                "Base instructions",
                /*tokens*/ 7_412,
            ),
            part(
                Tools,
                Some("exec"),
                "guidance and grammar",
                /*tokens*/ 3_120,
            ),
            part(Tools, Some("exec"), "apply_patch", /*tokens*/ 412),
            part(
                Tools,
                Some("mcp__depot"),
                "lsp_symbols",
                /*tokens*/ 611,
            ),
            part(
                Tools,
                Some("mcp__depot"),
                "sem_read_search",
                /*tokens*/ 1_904,
            ),
            part(Tools, /*group*/ None, "web_search", /*tokens*/ 21),
            part(
                Context,
                /*group*/ None,
                "environment_context",
                /*tokens*/ 1_560,
            ),
            part(
                Context,
                /*group*/ None,
                "AGENTS.md /repo",
                /*tokens*/ 9_734,
            ),
        ],
        change: Some(PreambleChange {
            previous_tokens: 23_000,
            added: vec!["mcp__depot/lsp_symbols".to_string()],
            removed: Vec::new(),
            resized: vec!["Base instructions".to_string()],
        }),
        reported_input_tokens: Some(31_208),
    });
    let text = visible_lines(cell.expanded_hyperlink_lines(/*width*/ 80))
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("preamble_expanded", text);
}
