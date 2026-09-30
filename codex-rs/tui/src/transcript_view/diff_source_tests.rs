//! Diff copy text retains code indentation and signs while gutters and soft wraps stay visual.

use super::*;
use crate::diff_model::FileChange;
use crate::history_cell::HistoryCell;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

#[test]
fn file_disclosures_are_independent_and_not_copied() {
    let cells: Vec<Arc<dyn HistoryCell>> = crate::history_cell::new_patch_events(
        HashMap::from([
            (
                PathBuf::from("alpha.rs"),
                FileChange::Add {
                    content: "alpha body\nsecond alpha line\n".into(),
                },
            ),
            (
                PathBuf::from("beta.rs"),
                FileChange::Add {
                    content: "beta body\nsecond beta line\n".into(),
                },
            ),
        ]),
        Path::new("/project"),
    )
    .into_iter()
    .map(|cell| Arc::new(cell.with_activity_id("patch-call".into())) as Arc<dyn HistoryCell>)
    .collect();
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
    let copy = "  beta.rs (+2, -0)\n+beta body\n+second beta line";
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
    let expected = "• Edited\n  test.rs (+2, -0)\n+    let message = \"a long value with 界 e\u{301} and 👩‍💻 inside\";\n+    return message;";
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
