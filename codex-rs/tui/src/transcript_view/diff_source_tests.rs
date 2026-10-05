//! Diff copy text retains code indentation and signs while gutters and soft wraps stay visual.

use super::*;
use crate::diff_model::FileChange;
use crate::history_cell::HistoryCell;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use crate::transcript_view::TranscriptView;
use codex_app_server_protocol::FileUpdateChange;
use codex_app_server_protocol::PatchApplyStatus;
use codex_app_server_protocol::PatchChangeKind;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::models::FunctionCallOutputBody;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn file_disclosures_are_independent_and_not_copied() {
    // The returned batch used to regroup all files into one disclosure, despite unique IDs.
    let returned = ThreadItem::FunctionCallOutput {
        id: "batch".into(),
        call_id: Some("patch-call".into()),
        name: "exec".into(),
        namespace: None,
        output: FunctionCallOutputBody::Text(
            "Script completed\nWall time 0.1 seconds\nOutput:\n{}".into(),
        ),
    };
    let cells = crate::thread_transcript::thread_items_to_transcript_cells(
        /*thread_id*/ None,
        &test_path_buf("/project").abs(),
        [
            ThreadItem::FileChange {
                id: "patch-call".into(),
                changes: vec![
                    FileUpdateChange {
                        path: "alpha.rs".into(),
                        kind: PatchChangeKind::Add,
                        diff: "alpha body\nsecond alpha line\n".into(),
                    },
                    FileUpdateChange {
                        path: "beta.rs".into(),
                        kind: PatchChangeKind::Add,
                        diff: "beta body\nsecond beta line\n".into(),
                    },
                ],
                status: PatchApplyStatus::Completed,
            },
            returned.clone(),
            returned,
        ],
        crate::thread_transcript::RawReasoningVisibility::Hidden,
        /*config*/ None,
    );
    let mut view = TranscriptView::default();
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 70, /*height*/ 10,
    );
    let mut snapshots = Vec::new();
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer, &cells);
    snapshots.push(format!("collapsed\n{buffer:?}"));
    let (row, column) = view
        .visible
        .iter()
        .enumerate()
        .find_map(|(row, visible)| {
            (visible.index == 1 && visible.layout.disclosure_row() == Some(visible.row))
                .then(|| (row as u16, visible.layout.disclosure_columns().start))
        })
        .expect("second file's inline control");
    assert!(view.toggle_disclosure_at(&cells, column, row));
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer, &cells);
    snapshots.push(format!("second file expanded\n{buffer:?}"));
    assert_eq!(
        cells
            .iter()
            .map(|cell| view.disclosure.is_expanded(&cell.activity_ids()))
            .collect::<Vec<_>>(),
        vec![false, true],
    );
    let layout = view
        .current_layout(&cells, /*index*/ 1)
        .expect("second file");
    let copy = "beta.rs (+2, -0)\n+beta body\n+second beta line";
    assert_eq!(layout.text(), copy);
    assert_eq!(layout.rewrap(/*width*/ 40).text(), copy);
    view.handle_key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyModifiers::NONE,
        ),
        &cells,
    );
    view.handle_key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
        &cells,
    );
    assert_eq!(
        cells
            .iter()
            .map(|cell| view.disclosure.is_expanded(&cell.activity_ids()))
            .collect::<Vec<_>>(),
        vec![true, true],
    );
    view.record_tool_output_tokens(
        "tool-output-tokens:batch".into(),
        "outer-exec".into(),
        Some(123),
    );
    assert_eq!(
        cells
            .iter()
            .map(|cell| view.activity_output_tokens(&cell.activity_ids()))
            .collect::<Vec<_>>(),
        vec![Some(123), None],
    );
    assert_eq!(
        view.current_layout(&cells, /*index*/ 0)
            .expect("first file")
            .text(),
        "• Edited\nalpha.rs (+2, -0)\n+alpha body\n+second alpha line",
    );
    for (label, collapse_first) in [
        ("both files expanded with batch bill", false),
        ("first file collapsed", true),
    ] {
        if collapse_first {
            assert!(view.toggle_disclosure_at(
                &cells,
                crate::transcript_geometry::DISCLOSURE_COLUMN,
                /*row*/ 1
            ));
        }
        view.clear_activity_focus();
        view.jump_to_latest();
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer, &cells);
        let text = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n");
        snapshots.push(format!("{label}\n{}", text.trim_end()));
    }
    assert_eq!(
        cells
            .iter()
            .map(|cell| view.disclosure.is_expanded(&cell.activity_ids()))
            .collect::<Vec<_>>(),
        vec![false, true],
    );
    // A later call shares the heading, not the prior call's disclosure or token bill.
    let mut cells = cells;
    cells.push(Arc::new(crate::history_cell::ReasoningSummaryCell::new(
        String::new(),
        "Hidden reasoning between edits".into(),
        Path::new("/project"),
        /*transcript_only*/ true,
    )));
    let next_index = cells.len();
    cells.extend(crate::thread_transcript::thread_items_to_transcript_cells(
        /*thread_id*/ None,
        &test_path_buf("/project").abs(),
        [
            ThreadItem::FileChange {
                id: "next-patch".into(),
                changes: vec![FileUpdateChange {
                    path: "gamma.rs".into(),
                    kind: PatchChangeKind::Add,
                    diff: "gamma body\n".into(),
                }],
                status: PatchApplyStatus::Completed,
            },
            ThreadItem::FunctionCallOutput {
                id: "next-batch".into(),
                call_id: Some("next-patch".into()),
                name: "exec".into(),
                namespace: None,
                output: FunctionCallOutputBody::Text("Next patch applied".into()),
            },
        ],
        crate::thread_transcript::RawReasoningVisibility::Hidden,
        /*config*/ None,
    ));
    view.record_tool_output_tokens(
        "tool-output-tokens:next-batch".into(),
        "next-exec".into(),
        Some(55),
    );
    view.disclosure.expanded.clear();
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 70, /*height*/ 14,
    );
    for stage in ["adjacent calls", "next file expanded", "message boundary"] {
        if stage == "message boundary" {
            cells.insert(
                next_index,
                Arc::new(crate::history_cell::PlainHistoryCell::new(vec![
                    "A separate message".into(),
                ])),
            );
        }
        view.clear_activity_focus();
        view.jump_to_latest();
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer, &cells);
        snapshots.push(format!(
            "{stage}\n{}",
            crate::transcript_view::tests::text(&buffer).trim_end()
        ));
        if stage == "adjacent calls" {
            let (row, column) = view
                .visible
                .iter()
                .enumerate()
                .find_map(|(row, visible)| {
                    (visible.index == next_index
                        && visible.layout.disclosure_row() == Some(visible.row))
                    .then(|| (row as u16, visible.layout.disclosure_columns().start))
                })
                .expect("next call's file control");
            assert!(view.toggle_disclosure_at(&cells, column, row));
            assert_eq!(
                cells
                    .iter()
                    .map(|cell| view.disclosure.is_expanded(&cell.activity_ids()))
                    .collect::<Vec<_>>(),
                vec![false, false, false, true],
            );
        }
    }
    insta::assert_snapshot!(snapshots.join("\n\n"));
}

#[test]
fn wrapped_added_code_keeps_indentation_and_graphemes_when_copied_and_resized() {
    let code =
        "\tlet message = \"a long value with 界 e\u{301} and 👩‍💻 inside\";\n    return message;\n";
    let patch = crate::history_cell::new_patch_events(
        HashMap::from([(
            PathBuf::from("test.rs"),
            FileChange::Add {
                content: code.to_string(),
            },
        )]),
        Path::new("/project"),
    )
    .remove(/*index*/ 0);
    let layout = TextLayout::new(
        patch.expanded_hyperlink_lines(/*width*/ 40),
        /*width*/ 40,
    );
    let expected = "• Edited\ntest.rs (+2, -0)\n+    let message = \"a long value with 界 e\u{301} and 👩‍💻 inside\";\n+    return message;";
    assert_eq!(layout.text(), expected);
    assert_eq!(layout.rewrap(/*width*/ 18).text(), expected);
    assert!(layout.row_count() > 3);
}

#[test]
fn annotated_diff_preserves_the_existing_hard_wrapped_visual_output() {
    let changes = HashMap::from([(
        PathBuf::from("test.rs"),
        FileChange::Add {
            content: "    let answer = calculate_the_result(first_argument, second_argument);\n"
                .to_string(),
        },
    )]);
    let lines = crate::history_cell::new_patch_events(changes, Path::new("/project"))
        .remove(/*index*/ 0)
        .display_hyperlink_lines(/*width*/ 40);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 40, /*height*/ 8,
    );
    let mut expected = Buffer::empty(area);
    HyperlinkParagraph::new(&lines, Style::default()).render(area, &mut expected);
    let mut actual = Buffer::empty(area);
    TextLayout::new(lines, /*width*/ 40).render(area, &mut actual, /*start_row*/ 0);
    assert_eq!(actual, expected);
}

#[test]
fn updated_diff_copy_includes_signs_but_no_line_number_gutters() {
    let patch = crate::history_cell::new_patch_events(
        HashMap::from([(PathBuf::from("example.txt"), FileChange::Update {
            unified_diff: "--- a/example.txt\n+++ b/example.txt\n@@ -1,2 +1,2 @@\n unchanged\n-old long value with several words\n+new long value with several words\n".to_string(),
            move_path: None,
        })]),
        Path::new("/project"),
    ).remove(/*index*/ 0);
    let layout = TextLayout::new(
        patch.expanded_hyperlink_lines(/*width*/ 20),
        /*width*/ 20,
    );
    let body = layout
        .text()
        .splitn(/*n*/ 3, '\n')
        .nth(/*n*/ 2)
        .expect("summary then diff");
    assert_eq!(
        body,
        " unchanged\n-old long value with several words\n+new long value with several words"
    );
}
