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
use codex_app_server_protocol::ThreadItemPayload;
use codex_protocol::models::FunctionCallOutputBody;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

#[test]
fn file_disclosures_are_independent_and_not_copied() {
    use crate::history_cell::activity_parts::FoldTarget;
    use crate::history_cell::invocation::Action;
    use crate::history_cell::invocation::Panel;
    use crate::history_cell::invocation::Port;
    use crate::history_cell::output_provenance::OutputMode;
    use codex_protocol::tool_input::ToolInput;
    use codex_protocol::tool_input::ToolInputPayload;
    let source = "*** Begin Patch\n*** Add File: alpha.rs\n+alpha body\n*** Add File: beta.rs\n+beta body\n*** End Patch";
    let returned = ThreadItemPayload::FunctionCallOutput {
        id: "batch".into(),
        call_id: Some("patch-call".into()),
        name: "apply_patch".into(),
        namespace: None,
        output: FunctionCallOutputBody::Text(r#"{"result":"patched"}"#.into()),
    };
    let cells = crate::thread_transcript::thread_items_to_transcript_cells(
        None,
        &test_path_buf("/project").abs(),
        [
            ThreadItemPayload::FileChange {
                id: "patch-call".into(),
                changes: ["alpha", "beta"]
                    .map(|name| FileUpdateChange {
                        path: format!("{name}.rs"),
                        kind: PatchChangeKind::Add,
                        diff: format!("{name} body\nsecond {name} line\n"),
                    })
                    .to_vec(),
                status: PatchApplyStatus::Completed,
            },
            returned.clone(),
            returned,
        ],
        crate::thread_transcript::RawReasoningVisibility::Hidden,
        None,
        &|_| crate::multi_agents::AgentMetadata::default(),
        crate::history_cell::ImagePathOrigin::Local,
    );
    assert_eq!(cells.len(), 1);
    let mut view = TranscriptView::default();
    view.record_tool_input(ToolInput {
        id: "input:patch-call".into(),
        call_id: "patch-call".into(),
        name: codex_protocol::ToolName::plain("apply_patch"),
        payload: ToolInputPayload::Text(source.into()),
    });
    view.record_tool_output_tokens(
        "tool-output-tokens:batch".into(),
        "patch-call".into(),
        Some(123),
    );
    let mut snapshots = Vec::new();
    let mut paint = |view: &mut TranscriptView, width, label: &str| {
        view.jump_to_latest();
        let buffer = crate::transcript_view::tests::render(view, &cells, width, 20);
        snapshots.push(format!(
            "{label}\n{}",
            crate::transcript_view::tests::text(&buffer).trim_end()
        ));
        view.current_layout(&cells, 0).unwrap()
    };
    let control = |action| FoldTarget::Invocation {
        call_id: "patch-call".into(),
        action,
    };
    let layout = paint(&mut view, 104, "source and file groups");
    assert!(layout.text().contains(source));
    assert!(
        !layout
            .fold_targets()
            .contains(&control(Action::Format(Port::Input, OutputMode::Fmt)))
    );
    assert!(view.focus_fold(
        &cells,
        0,
        control(Action::Toggle(Port::Input, Panel::Hidden(OutputMode::Raw)))
    ));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    let layout = paint(&mut view, 104, "file groups without ports");
    assert!(layout.text().contains("Edited"));
    assert!(!layout.text().contains("beta body"));
    let files: Vec<_> = layout
        .fold_targets()
        .into_iter()
        .filter(|target| matches!(target, FoldTarget::GroupedPart { .. }))
        .collect();
    assert_eq!(files.len(), 2);
    crate::transcript_view::tests::render(&mut view, &cells, 104, 4);
    assert!(view.focus_fold(&cells, 0, files[1].clone()));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    // The old paint helper jumped to latest and concealed disclosure-induced loss of follow.
    assert!(view.is_following());
    let expanded = crate::transcript_view::tests::render(&mut view, &cells, 104, 4);
    assert!(crate::transcript_view::tests::text(&expanded).contains("+second beta line"));
    assert!(view.tail_visible);
    let layout = paint(&mut view, 104, "second file");
    assert!(layout.text().contains("+beta body"));
    assert!(!layout.text().contains("+alpha body"));
    assert!(
        !layout
            .text()
            .contains(crate::render::disclosure::glyph(false).trim())
    );
    assert_eq!(layout.text(), layout.rewrap(40).text());
    for panel in [
        Panel::Visible(OutputMode::Raw),
        Panel::Hidden(OutputMode::Raw),
    ] {
        assert!(view.focus_fold(&cells, 0, control(Action::Toggle(Port::Input, panel))));
        view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
        let layout = paint(&mut view, 80, "input visibility preserves file folds");
        assert_eq!(layout.text().contains(source), panel.visible());
        assert!(layout.text().contains("+second beta line"));
        assert!(!layout.text().contains("+second alpha line"));
    }
    assert!(view.focus_fold(&cells, 0, files[0].clone()));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    let layout = paint(&mut view, 104, "both files");
    assert!(layout.text().contains("+alpha body"));
    assert!(layout.text().contains("+beta body"));
    assert!(view.focus_fold(
        &cells,
        0,
        control(Action::Toggle(
            Port::Output,
            Panel::Visible(OutputMode::Fmt)
        ))
    ));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    paint(&mut view, 104, "output and edits");
    for mode in [OutputMode::Raw, OutputMode::Fmt] {
        assert!(view.focus_fold(&cells, 0, control(Action::Format(Port::Output, mode))));
        view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
        let layout = paint(&mut view, 104, "output format preserves file folds");
        assert!(layout.text().contains('{'));
        assert!(layout.text().contains("\"patched\""));
        assert!(layout.text().contains("Edited"));
        assert!(layout.text().contains("+alpha body"));
        assert!(layout.text().contains("+beta body"));
    }
    assert_eq!(
        view.activity_output_tokens(&cells[0].activity_ids()),
        Some(123)
    );
    insta::assert_snapshot!(snapshots.join("\n\n"));

    view.jump_to_latest();
    crate::transcript_view::tests::render(&mut view, &cells, 104, 4);
    assert!(view.focus_fold(&cells, 0, files[1].clone()));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    assert!(view.is_following());
    crate::transcript_view::tests::render(&mut view, &cells, 104, 4);
    let (row, column, _) = view
        .current_layout(&cells, 0)
        .unwrap()
        .fold_position(&files[1])
        .unwrap();
    let screen_row = view
        .visible
        .iter()
        .position(|visible| visible.row == row)
        .unwrap();
    assert!(view.toggle_disclosure_at(&cells, column.start, screen_row as u16));
    assert!(view.is_following());
    let expanded = crate::transcript_view::tests::render(&mut view, &cells, 104, 4);
    assert!(crate::transcript_view::tests::text(&expanded).contains("+second beta line"));
    assert!(view.tail_visible);

    view.scroll(&cells, -1);
    crate::transcript_view::tests::render(&mut view, &cells, 104, 4);
    assert!(view.focus_fold(&cells, 0, files[1].clone()));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    assert!(!view.is_following());
}

#[test]
fn grouped_file_disclosures_span_receipts_but_not_feed_items_or_ports() {
    use crate::history_cell::activity_parts::FoldGroup;
    use crate::history_cell::activity_parts::FoldTarget;
    use crossterm::event::KeyModifiers;
    use crossterm::event::MouseButton;
    use crossterm::event::MouseEvent;
    use crossterm::event::MouseEventKind;
    fn patch(id: &str) -> std::sync::Arc<dyn HistoryCell> {
        crate::thread_transcript::thread_items_to_transcript_cells(
            None,
            &test_path_buf("/project").abs(),
            [ThreadItemPayload::FileChange {
                id: id.into(),
                changes: ["alpha", "beta"]
                    .map(|file| FileUpdateChange {
                        path: format!("{id}-{file}.rs"),
                        kind: PatchChangeKind::Add,
                        diff: format!("{id}-{file} body\n"),
                    })
                    .to_vec(),
                status: PatchApplyStatus::Completed,
            }],
            crate::thread_transcript::RawReasoningVisibility::Hidden,
            None,
            &|_| crate::multi_agents::AgentMetadata::default(),
            crate::history_cell::ImagePathOrigin::Local,
        )
        .remove(0)
    }
    let cells = vec![patch("a"), patch("b"), patch("other")];
    let mut view = TranscriptView::default();
    for (response, calls) in [("edits", vec!["a", "b"]), ("other", vec!["other"])] {
        view.record_step_admission(codex_app_server_protocol::ThreadItem {
            payload: codex_app_server_protocol::ThreadItemPayload::StepAdmission(
                codex_app_server_protocol::StepAdmissionItem {
                    id: format!("step:{response}"),
                    response_id: response.into(),
                    read_tokens: 0,
                    thinking_tokens: None,
                    output_tokens: 0,
                    call_ids: calls.into_iter().map(str::to_owned).collect(),
                    message_ids: vec![],
                    admission: codex_protocol::step_admission::Admission::Pending,
                },
            ),
            started_at_ms: None,
            completed_at_ms: None,
        });
    }
    crate::transcript_view::tests::render(&mut view, &cells, 104, 80);
    let files = |view: &mut TranscriptView, index| {
        view.current_layout(&cells, index)
            .unwrap()
            .fold_targets()
            .into_iter()
            .filter(|target| {
                matches!(
                    target,
                    FoldTarget::GroupedPart {
                        group: FoldGroup::Diff,
                        ..
                    }
                )
            })
            .collect::<Vec<_>>()
    };
    let first = files(&mut view, 0).remove(0);
    assert_eq!(files(&mut view, 0).len(), 4);
    assert!(files(&mut view, 1).is_empty());
    for (index, headings) in [(0, 1), (1, 0), (2, 1)] {
        assert_eq!(
            view.current_layout(&cells, index)
                .unwrap()
                .text()
                .matches("Edited")
                .count(),
            headings,
        );
    }
    let click = |view: &mut TranscriptView, target: &FoldTarget| {
        crate::transcript_view::tests::render(view, &cells, 104, 80);
        let layout = view.current_layout(&cells, 0).unwrap();
        let (row, columns, _) = layout.fold_position(target).unwrap();
        let screen = view
            .visible
            .iter()
            .position(|visible| visible.index == 0 && visible.row == row)
            .unwrap();
        assert!(matches!(
            view.handle_mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: columns.start,
                    row: screen as u16,
                    modifiers: KeyModifiers::CONTROL,
                },
                &cells
            ),
            Some(crate::transcript_view::input::ViewAction::Changed)
        ));
        crate::transcript_view::tests::render(view, &cells, 104, 80);
    };
    click(&mut view, &first);
    let layout = view.current_layout(&cells, 0).unwrap();
    for call in ["a", "b"] {
        assert!(layout.text().contains(&format!("{call}-alpha body")));
        assert!(layout.text().contains(&format!("{call}-beta body")));
    }
    assert!(
        !view
            .current_layout(&cells, 2)
            .unwrap()
            .text()
            .contains("alpha body")
    );
    let exception = files(&mut view, 0).remove(2);
    assert!(view.focus_fold(&cells, 0, exception));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    crate::transcript_view::tests::render(&mut view, &cells, 104, 80);
    assert!(
        !view
            .current_layout(&cells, 0)
            .unwrap()
            .text()
            .contains("b-alpha body")
    );
    click(&mut view, &first); // Closing replaces the individual exception too.
    for index in [0, 1] {
        assert!(
            !view
                .current_layout(&cells, index)
                .unwrap()
                .text()
                .contains("beta body")
        );
    }
    // An already prepared worker's scoped policy must not follow later viewer actions.
    let closed_snapshot = view.disclosure.expanded.scoped(view.feed.fold_scope(0));
    click(&mut view, &first);
    let FoldTarget::GroupedPart { id, group } = &first else {
        unreachable!("file disclosure")
    };
    assert!(!closed_snapshot.is_open(*group, id, false));
    assert!(
        view.current_layout(&cells, 0)
            .unwrap()
            .text()
            .contains("b-alpha body")
    );
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
    for (row, line) in lines.iter().enumerate() {
        for column in 0..area.width {
            if expected[(column, row as u16)].symbol() == " " {
                expected[(column, row as u16)].set_style(line.line.style);
            }
        }
    }
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
