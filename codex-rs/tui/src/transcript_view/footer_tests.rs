//! Loading feedback preserves the transcript's selection, search editor, and passive context.

use super::*;
use crate::history_cell::PlainHistoryCell;
use crate::style::accent_color;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

#[test]
fn selection_at_bottom_restores_footer_without_resuming_following() {
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(PlainHistoryCell::new(vec![
        "one".into(),
        "two".into(),
        "three".into(),
        "four".into(),
        "needle final".into(),
    ]))];
    let mut observations = Vec::new();
    for history in [
        TranscriptHistoryState::Partial,
        TranscriptHistoryState::Complete,
    ] {
        let mut view = TranscriptView {
            history,
            ..TranscriptView::default()
        };
        let initial = render(&mut view, &cells);
        assert!(view.tail_visible);
        assert!(view.is_following());
        assert!(view.footer(/*width*/ 120, MotionMode::Reduced).is_none());

        for (label, events) in [
            (
                "selected at bottom",
                vec![
                    (MouseEventKind::Down(MouseButton::Left), 0, 2),
                    (MouseEventKind::Drag(MouseButton::Left), 6, 2),
                    (MouseEventKind::Up(MouseButton::Left), 6, 2),
                ],
            ),
            (
                "clicked away",
                vec![
                    (MouseEventKind::Down(MouseButton::Left), 0, 1),
                    (MouseEventKind::Up(MouseButton::Left), 0, 1),
                ],
            ),
            (
                "scrolled down at bottom",
                vec![(MouseEventKind::ScrollDown, 0, 1)],
            ),
        ] {
            let mut buffer = initial.clone();
            for (kind, column, row) in events {
                view.handle_mouse(
                    MouseEvent {
                        kind,
                        column,
                        row,
                        modifiers: KeyModifiers::NONE,
                    },
                    &cells,
                );
                buffer = render(&mut view, &cells);
            }
            assert!(view.tail_visible);
            assert_eq!(
                crate::transcript_view::tests::text(&buffer),
                crate::transcript_view::tests::text(&initial),
            );
            let selected = label == "selected at bottom";
            assert_eq!(
                view.selected_text(&cells).as_deref(),
                selected.then_some("needle"),
            );
            assert_eq!(view.is_following(), label == "scrolled down at bottom");
            let footer = view.footer(/*width*/ 120, MotionMode::Reduced);
            let hint = footer.map_or_else(
                || "<composer hints>".to_owned(),
                |footer| footer.text.to_string(),
            );
            observations.push(format!("{history:?}, {label}: {hint}"));
        }
        view.scroll(&cells, /*rows*/ -1);
        render(&mut view, &cells);
        view.begin_selection(&cells, /*column*/ 0, /*row*/ 0, /*clicks*/ 3);
        view.end_drag();
        render(&mut view, &cells);
        assert!(!view.tail_visible);
        assert_eq!(view.selected_text(&cells).as_deref(), Some("two\n"));
        let footer = view.footer(/*width*/ 120, MotionMode::Reduced).unwrap();
        observations.push(format!(
            "{history:?}, selected above bottom: {}",
            footer.text
        ));
    }
    insta::assert_snapshot!(observations.join("\n"), @"
    Partial, selected at bottom: ctrl+c copy · enter copy & follow · esc clear
    Partial, clicked away: <composer hints>
    Partial, scrolled down at bottom: <composer hints>
    Partial, selected above bottom: Earlier messages available.  ctrl+c copy · enter copy & follow · esc clear
    Complete, selected at bottom: ctrl+c copy · enter copy & follow · esc clear
    Complete, clicked away: <composer hints>
    Complete, scrolled down at bottom: <composer hints>
    Complete, selected above bottom: ctrl+c copy · enter copy & follow · esc clear
    ");
}

fn render(view: &mut TranscriptView, cells: &[Arc<dyn HistoryCell>]) -> Buffer {
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 32, /*height*/ 3,
    );
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer, cells);
    buffer
}

#[test]
fn selection_hints_follow_selected_text_instead_of_mouse_down() {
    let cells: Vec<Arc<dyn HistoryCell>> =
        vec![Arc::new(PlainHistoryCell::new(vec!["é selection".into()]))];
    let mut snapshots = Vec::new();
    for searching in [false, true] {
        let mut view = TranscriptView::default();
        render(&mut view, &cells);
        if searching {
            view.begin_search();
        }
        for (label, kind, column) in [
            ("mouse down", MouseEventKind::Down(MouseButton::Left), 0),
            ("one character", MouseEventKind::Drag(MouseButton::Left), 1),
            ("back to anchor", MouseEventKind::Drag(MouseButton::Left), 0),
            ("released empty", MouseEventKind::Up(MouseButton::Left), 0),
        ] {
            view.handle_mouse(
                MouseEvent {
                    kind,
                    column,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                },
                &cells,
            );
            render(&mut view, &cells);
            let footer = view.footer(/*width*/ 80, MotionMode::Reduced);
            if label == "one character" {
                assert_eq!(view.selected_text(&cells).as_deref(), Some("é"));
            }
            let text = footer.map_or_else(
                || "<composer hints>".to_string(),
                |footer| footer.text.to_string(),
            );
            snapshots.push(format!("searching={searching} · {label}\n{text}"));
        }
    }
    let joined = snapshots.join("\n\n");
    let snapshot = joined
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("selection_hint_threshold", snapshot);
}

#[test]
fn loading_preserves_selected_content_and_copy_action() {
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(PlainHistoryCell::new(vec![
        "selected words".into(),
    ]))];
    let mut view = TranscriptView::default();
    render(&mut view, &cells);
    view.begin_selection(&cells, /*column*/ 0, /*row*/ 0, /*clicks*/ 3);
    view.end_drag();
    let before = render(&mut view, &cells);
    let position = view.position;

    for state in [
        TranscriptHistoryState::LoadingOlder,
        TranscriptHistoryState::LoadingBeginning,
    ] {
        view.history = state;
        let footer = view
            .footer(/*width*/ 80, MotionMode::Reduced)
            .expect("loading selection footer");
        assert_eq!((footer.cursor_column, footer.is_interactive), (None, true));
        insta::allow_duplicates! {
            insta::assert_snapshot!(footer.text.to_string(), @"↑ Loading earlier messages… · ctrl+c copy · enter copy & follow · esc clear");
        }
        let Some(ViewAction::Copy(copied)) = view.handle_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &cells,
        ) else {
            panic!("copy selection while loading");
        };
        assert_eq!(copied, "selected words");
        assert_eq!(
            (render(&mut view, &cells), view.position),
            (before.clone(), position)
        );
    }
    view.history = TranscriptHistoryState::Complete;
    let footer = view
        .footer(/*width*/ 80, MotionMode::Reduced)
        .expect("selection footer after loading");
    insta::assert_snapshot!(footer.text.to_string(), @"ctrl+c copy · enter copy & follow · esc clear");
    assert_eq!(
        view.selected_text(&cells),
        Some("selected words".to_string())
    );
    view.history = TranscriptHistoryState::Failed;
    assert_eq!(
        view.footer(/*width*/ 120, MotionMode::Reduced)
            .unwrap()
            .text
            .to_string(),
        format!(
            "ctrl+c copy · enter copy & follow · esc clear · Retry: {}",
            JumpTarget::Beginning.hint_label(),
        ),
    );
    insta::assert_snapshot!(view.footer(/*width*/ 38, MotionMode::Reduced).unwrap().text.to_string(),
        @"enter copy & follow · esc clear");
}

#[test]
fn loading_and_failure_preserve_search_query_and_caret() {
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(PlainHistoryCell::new(vec![
        "visible history".into(),
    ]))];
    let mut view = TranscriptView::default();
    render(&mut view, &cells);
    view.history = TranscriptHistoryState::Partial;
    view.begin_search();
    view.paste_search("missing needle");
    assert!(!view.advance_search(&cells));
    assert!(view.needs_history(&cells));

    let mut hints = Vec::new();
    for width in [32, 12] {
        let editor = view.search_footer(width).expect("search editor");
        for state in [
            TranscriptHistoryState::LoadingOlder,
            TranscriptHistoryState::LoadingBeginning,
            TranscriptHistoryState::Failed,
        ] {
            view.history = state;
            view.advance_search(&cells);
            let footer = view
                .footer(width, MotionMode::Reduced)
                .expect("search footer");
            assert_eq!(
                (footer.text.lines[0].clone(), footer.cursor_column),
                (editor.0.clone(), Some(editor.1)),
            );
            assert!(footer.is_interactive);
            let hint = &footer.text.lines[1];
            assert!(hint.width() <= usize::from(width));
            hints.push(format!("{width} columns, {state:?}: {hint}"));
        }
    }
    insta::assert_snapshot!(hints.join("\n"));
    view.history = TranscriptHistoryState::Complete;
    view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &cells);
    assert!(!view.is_search_active());
    assert!(!view.is_detailed());
    assert!(view.footer(/*width*/ 32, MotionMode::Reduced).is_none());
}

#[test]
fn loading_completion_restores_unseen_activity_and_failure_stops_motion() {
    let mut cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(PlainHistoryCell::new(vec![
        "one".into(),
        "two".into(),
        "three".into(),
        "four".into(),
    ]))];
    let mut view = TranscriptView::default();
    render(&mut view, &cells);
    view.scroll(&cells, /*rows*/ -1);
    cells.push(Arc::new(PlainHistoryCell::new(vec!["new output".into()])));
    render(&mut view, &cells);
    let before = view
        .footer(/*width*/ 80, MotionMode::Reduced)
        .expect("unseen activity footer")
        .text;
    view.history = TranscriptHistoryState::LoadingOlder;
    let loading = view
        .footer(/*width*/ 80, MotionMode::Reduced)
        .expect("loading footer");
    assert_eq!(loading.text.lines[0].spans[0], "↑ ".fg(accent_color()));
    insta::assert_snapshot!(loading.text.to_string(), @"↑ Loading earlier messages… · New activity · esc latest");
    view.history = TranscriptHistoryState::Failed;
    assert!(!view.is_loading_history());
    let failed = view
        .footer(/*width*/ 80, MotionMode::Reduced)
        .expect("failed history footer");
    insta::assert_snapshot!(failed.text.to_string(), @"New activity · Retry history: ⌥+</ctrl+home.  esc latest");
    view.history = TranscriptHistoryState::Complete;
    assert!(!view.is_loading_history());
    assert_eq!(
        view.footer(/*width*/ 80, MotionMode::Reduced)
            .expect("restored unseen activity footer")
            .text,
        before,
    );
    view.jump_to_latest();
    assert!(view.footer(/*width*/ 80, MotionMode::Reduced).is_none());
}

#[test]
fn passive_footer_keeps_jump_actions_in_one_row() {
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(PlainHistoryCell::new(vec![
        "retained history".into(),
    ]))];
    let mut snapshots = Vec::new();
    for width in [26, 38, 78] {
        for history in [
            TranscriptHistoryState::LoadingOlder,
            TranscriptHistoryState::LoadingBeginning,
            TranscriptHistoryState::Failed,
            TranscriptHistoryState::Complete,
        ] {
            let mut view = TranscriptView::default();
            view.jump_to_entry(&cells, /*index*/ 0);
            view.history = history;
            view.unseen_activity = true;
            let footer = view
                .footer(width, MotionMode::Reduced)
                .expect("activity footer");
            assert_eq!(footer.text.height(), 1);
            assert!(footer.text.width() <= usize::from(width));
            snapshots.push(format!("{width} columns, {history:?}: {}", footer.text));
        }
    }
    insta::assert_snapshot!("compact_navigation_hints", snapshots.join("\n"));
}

#[test]
fn narrow_selection_and_search_keep_complete_exit_hints() {
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(PlainHistoryCell::new(vec![
        "selected words".into(),
    ]))];
    let mut view = TranscriptView::default();
    render(&mut view, &cells);
    view.begin_selection(&cells, /*column*/ 0, /*row*/ 0, /*clicks*/ 3);
    view.end_drag();
    let mut snapshots = Vec::new();
    for searching in [false, true] {
        if searching {
            view.end_selection(&cells);
            view.begin_search();
            view.paste_search("selected");
            view.advance_search(&cells);
        }
        for width in [12, 20, 32, 80] {
            let footer = view.footer(width, MotionMode::Reduced).unwrap();
            let hint = footer.text.lines.last().unwrap();
            assert!(hint.width() <= usize::from(width));
            assert!(hint.to_string().contains("esc"));
            snapshots.push(format!("search={searching}, {width} columns: {hint}"));
        }
    }
    insta::assert_snapshot!(snapshots.join("\n"));
}

#[derive(Debug, Default)]
struct ActivityCell {
    expanded_renders: AtomicUsize,
}

impl HistoryCell for ActivityCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec!["Activity summary".into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.transcript_lines(/*width*/ 80)
    }

    fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec!["Activity summary".into(), "Hidden details".into()]
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.expanded_renders
            .fetch_add(/*val*/ 1, Ordering::Relaxed);
        self.transcript_hyperlink_lines(width)
    }

    fn activity_ids(&self) -> Vec<String> {
        vec!["exec:sample-activity".into()]
    }
}

#[test]
fn code_mode_batches_display_their_own_counts_and_replay_without_double_counting() {
    use crate::test_support::PathBufExt;
    use codex_app_server_protocol::ThreadItem;
    use codex_protocol::models::FunctionCallOutputBody;

    let cwd = crate::test_support::test_path_buf("/project").abs();
    let items = ["first", "second"].map(|id| ThreadItem::FunctionCallOutput {
        id: id.into(),
        name: "exec".into(),
        namespace: None,
        output: FunctionCallOutputBody::Text(format!(
            "{id} returned batch\nOnly emitted text, after truncation."
        )),
    });
    // Use production history projection: counts belong to outer outputs, not
    // the differently identified commands hidden inside functions.exec.
    let cells = crate::thread_transcript::thread_items_to_transcript_cells(
        /*thread_id*/ None,
        &cwd,
        items,
        crate::thread_transcript::RawReasoningVisibility::Hidden,
        /*config*/ None,
    );
    assert_eq!(cells.len(), 2);
    let mut view = TranscriptView::default();
    view.set_output_estimates_enabled(/*enabled*/ true);
    for (id, tokens) in [("first", 1_000), ("second", 280), ("first", 1_000)] {
        view.record_tool_output_tokens(
            format!("tool-output-tokens:{id}"),
            "outer-exec".into(),
            Some(tokens),
        );
    }
    assert_eq!(
        view.activity_output_tokens(&cells[0].activity_ids()),
        Some(1_000)
    );
    assert_eq!(
        view.activity_output_tokens(&cells[1].activity_ids()),
        Some(280)
    );
    assert_eq!(
        view.activity_output_tokens(&["exec:outer-exec".into()]),
        Some(1_280)
    );
    let mut snapshots = Vec::new();
    for expanded in [false, true] {
        if expanded {
            view.disclosure
                .expanded
                .extend(cells.iter().flat_map(|cell| cell.activity_ids()));
        }
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 10,
        );
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer, &cells);
        snapshots.push(format!("expanded={expanded}\n{buffer:?}"));

        // Committing a live tool entry must not change the gap above its header.
        let committed = &cells[..1];
        let tail = &cells[1];
        let ids = tail.activity_ids();
        let tool_output_tokens = view.activity_output_tokens(&ids);
        view.sync_live_activity(committed, ids);
        view.sync_live_activity_tail(/*width*/ 80, /*key*/ None, expanded, |width| {
            Some(ActivityTranscriptLines {
                activity: if expanded {
                    tail.expanded_hyperlink_lines(width)
                } else {
                    tail.compact_hyperlink_lines(width)
                },
                auxiliary: Vec::new(),
                tool_output_tokens,
                disclosure: tail.activity_disclosure(width),
            })
        });
        let mut live_buffer = Buffer::empty(area);
        view.render(area, &mut live_buffer, committed);
        assert_eq!(live_buffer, buffer);
        view.sync_live_activity(&cells, Vec::new());
        view.sync_live_tail(/*width*/ 80, /*key*/ None, |_| None);
    }
    let mut surrounded: Vec<Arc<dyn HistoryCell>> =
        vec![Arc::new(PlainHistoryCell::new(vec!["Before tools".into()]))];
    surrounded.extend(cells);
    surrounded.push(Arc::new(PlainHistoryCell::new(vec!["After tools".into()])));
    view.disclosure.expanded.clear();
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 10,
    );
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer, &surrounded);
    snapshots.push(format!("message boundaries\n{buffer:?}"));
    insta::assert_snapshot!("code_mode_returned_batch_counts", snapshots.join("\n"));
}

#[test]
fn mixed_shell_reads_share_one_disclosure_and_output_bill() {
    use crate::exec_cell::CommandOutput;
    use crate::exec_cell::new_active_exec_command;
    use codex_app_server_protocol::CommandExecutionSource;
    use codex_shell_command::parse_command::parse_command;
    use std::time::Duration;

    let command = vec![
        "bash".into(),
        "-lc".into(),
        "git status; sed -n '20,80p' src/lib.rs; tail -n 10 log.txt".into(),
    ];
    let parsed = parse_command(&command);
    let mut cell = new_active_exec_command(
        "mixed".into(),
        command,
        parsed,
        CommandExecutionSource::UnifiedExecStartup,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    // Recognized reads must not hide the opaque stage or turn the whole script into
    // a read-only exploration group. The process has one result and one output bill.
    assert!(!cell.is_exploring_cell());
    cell.complete_call(
        "mixed",
        CommandOutput::new(/*exit_code*/ 2, "combined output\nfinal error".into()),
        Duration::from_millis(/*millis*/ 5),
    );
    assert!(cell.should_flush());
    assert_eq!(cell.activity_ids(), vec!["exec:mixed"]);
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(cell)];
    let mut view = TranscriptView::default();
    view.set_output_estimates_enabled(/*enabled*/ true);
    for _ in 0..2 {
        view.record_tool_output_tokens("bill".into(), "mixed".into(), Some(1_280));
    }
    assert_eq!(
        view.activity_output_tokens(&cells[0].activity_ids()),
        Some(1_280)
    );
    let mut snapshots = Vec::new();
    for expanded in [false, true] {
        if expanded {
            view.disclosure.expanded.extend(cells[0].activity_ids());
        }
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 100, /*height*/ 10,
        );
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer, &cells);
        let text = crate::transcript_view::tests::text(&buffer);
        assert_eq!(text.matches("(~ 1,280 tokens)").count(), 1);
        assert_eq!(
            text.matches(if expanded { "[-]" } else { "[+]" }).count(),
            1
        );
        snapshots.push(format!("expanded={expanded}\n{buffer:?}"));
    }
    insta::assert_snapshot!("mixed_shell_reads_shared_output", snapshots.join("\n"));
}

#[test]
fn activity_focus_keeps_controls_without_passive_hints() {
    let mut cells: Vec<Arc<dyn HistoryCell>> = Vec::new();
    let cell = Arc::new(ActivityCell::default());
    let mut view = TranscriptView::default();
    view.set_output_estimates_enabled(true);
    let render_live = |view: &mut TranscriptView| {
        let expanded = view.sync_live_activity(&cells, cell.activity_ids());
        let tool_output_tokens = view.activity_output_tokens(&cell.activity_ids());
        view.sync_live_activity_tail(/*width*/ 32, /*key*/ None, expanded, |width| {
            Some(ActivityTranscriptLines {
                activity: if expanded {
                    cell.expanded_hyperlink_lines(width)
                } else {
                    cell.compact_hyperlink_lines(width)
                },
                auxiliary: Vec::new(),
                tool_output_tokens,
                disclosure: cell.activity_disclosure(width),
            })
        });
        render(view, &[])
    };
    let pending = render_live(&mut view);
    // Multiple outputs belong to one call; replaying the same measurement must not double it.
    for (id, tokens) in [("first", 1_000), ("second", 280), ("first", 1_000)] {
        view.record_tool_output_tokens(id.into(), "sample-activity".into(), Some(tokens));
    }
    let collapsed = render_live(&mut view);
    let mut hints = vec![format!(
        "awaiting measurement\n{pending:?}\ncollapsed\n{collapsed:?}"
    )];
    for width in [20, 32, 80] {
        hints.push(format!(
            "{width} columns: {}",
            view.footer(width, MotionMode::Reduced).map_or_else(
                || "<composer hints>".to_owned(),
                |footer| footer.text.to_string()
            ),
        ));
    }
    view.handle_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE), &cells);
    assert!(view.is_activity_focused());
    hints.push(format!("focused\n{:?}", render_live(&mut view)));
    for width in [20, 32, 80] {
        let footer = view.footer(width, MotionMode::Reduced).unwrap();
        assert!(footer.is_interactive);
        hints.push(format!("focused, {width} columns: {}", footer.text));
    }
    view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &cells);
    assert!(view.disclosure.is_expanded(&cell.activity_ids()));
    let expanded = render_live(&mut view);
    hints.push(format!("expanded\n{expanded:?}"));
    let hidden = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 32, /*height*/ 0,
    );
    view.render(hidden, &mut Buffer::empty(hidden), &cells);
    cells.push(cell.clone());
    view.sync_live_activity(&cells, Vec::new());
    view.sync_live_tail(/*width*/ 32, /*key*/ None, |_| None);
    assert_eq!(render(&mut view, &cells), expanded);
    assert!(cell.expanded_renders.swap(/*val*/ 0, Ordering::Relaxed) > 0);
    view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &cells);
    render(&mut view, &cells);
    assert_eq!(cell.expanded_renders.load(Ordering::Relaxed), 0);
    view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &cells);
    assert!(!view.is_activity_focused());

    for configured in [
        serde_json::json!("ctrl-x t"),
        serde_json::json!(["ctrl-x t", "f4"]),
        serde_json::json!([]),
    ] {
        let config = serde_json::from_value(serde_json::json!({
            "global": {"focus_activity": configured}
        }))
        .unwrap();
        let keymap = crate::keymap::RuntimeKeymap::from_config(&config).unwrap();
        view.set_keymap_bindings(&keymap);
        view.jump_to_latest();
        render(&mut view, &cells);
        let hint = view.footer(/*width*/ 80, MotionMode::Reduced).map_or_else(
            || "<composer hints>".to_owned(),
            |footer| footer.text.to_string(),
        );
        hints.push(format!("{configured}: {hint}"));
    }
    insta::assert_snapshot!(hints.join("\n"));
}

#[test]
fn transcript_footer_keys_remain_distinct_from_prose_and_query_text() {
    use ratatui::widgets::Paragraph;

    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(ActivityCell::default())];
    let mut view = TranscriptView::default();
    render(&mut view, &cells);
    view.begin_search();
    view.paste_search("Activity");
    view.advance_search(&cells);
    let footer = view.footer(/*width*/ 80, MotionMode::Reduced).unwrap();
    let area = Rect::new(
        /*x*/ 0,
        /*y*/ 0,
        /*width*/ 80,
        footer.text.height() as u16,
    );
    let mut buffer = Buffer::empty(area);
    Paragraph::new(footer.text).render(area, &mut buffer);
    insta::assert_snapshot!(format!("{buffer:?}"));
}

impl TranscriptView {
    pub(crate) fn footer(&self, width: u16, motion: MotionMode) -> Option<TranscriptFooter> {
        self.footer_with_navigation(width, motion, "esc latest")
    }
}
