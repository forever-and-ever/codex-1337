//! The feed's one interaction worth pinning: steps fold under their headers with their figures,
//! a Code Mode header shows its block's script, and an activity whose only row became its
//! header keeps its details behind its own control. Preamble summaries and retained raw context
//! have independent folds across navigation, reflow, replay, and full-source search.

use super::*;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::new_active_exec_command;
use crate::history_cell::ThoughtTextVisibility;
use codex_app_server_protocol::ArrivalItem;
use codex_app_server_protocol::CodeModeEvent;
use codex_app_server_protocol::CodeModeItem;
use codex_app_server_protocol::CodeModeOwner;
use codex_app_server_protocol::CodeModeStatus;
use codex_app_server_protocol::CommandExecutionSource;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::ThreadId;
use codex_protocol::step_admission::Admission;
use codex_protocol::step_admission::AdmissionPrecision;
use codex_protocol::step_admission::Arrival;
use codex_protocol::step_admission::ArrivalKind;
use codex_protocol::step_admission::SealedAdmission;

const CLOCK_MS: i64 = 1_700_000_000_000;

fn thought(response: &str) -> Arc<dyn HistoryCell> {
    Arc::new(ThoughtCell::from_item(
        ThreadItem::Thought {
            id: format!("thought:{response}"),
            response_id: response.into(),
            completed_at_ms: CLOCK_MS,
            summary: vec!["Considering the failure".into()],
            content: Vec::new(),
            reported_reasoning_tokens: None,
        },
        std::path::Path::new("/tmp"),
        ThoughtTextVisibility::Summary,
    ))
}

fn message(id: &str, text: &str) -> Arc<dyn HistoryCell> {
    let mut cell = AgentMarkdownCell::new(text.into(), std::path::Path::new("/tmp"));
    cell.item_id = Some(id.into());
    Arc::new(cell)
}

/// A finished Code Mode block that kept its script, with the one call the script made.
fn script_block(exec: &str) -> Arc<dyn HistoryCell> {
    let fact = |suffix: &str, event| {
        ThreadItem::CodeMode(CodeModeItem {
            id: format!("code-mode:{exec}:{suffix}"),
            cell_id: None,
            exec_call_id: exec.into(),
            created_at: CLOCK_MS / 1_000,
            event,
        })
    };
    let owner = CodeModeOwner {
        thread_id: ThreadId::from_u128(/*value*/ 7),
        exec_call_id: exec.into(),
        cell_id: None,
        started_at_ms: CLOCK_MS,
    };
    let cwd = codex_utils_absolute_path::AbsolutePathBuf::current_dir().unwrap();
    let mut reducers = crate::code_mode_transcript::Reducers::default();
    let mut reduce = |item| {
        reducers
            .admit(
                owner.thread_id,
                crate::code_mode_transcript::Admission::item(
                    codex_app_server_protocol::CodeModeOwned::new(owner.clone(), Box::new(item)),
                    String::new(),
                ),
                &cwd,
                /*animations*/ false,
                &|_| crate::multi_agents::AgentMetadata::default(),
            )
            .unwrap()
            .block
    };
    reduce(fact("start", CodeModeEvent::Started {
        call_id: exec.into(),
        script: Some(
            "const status = await tools.exec_command({ cmd: \"git status\" });\ntext(status.output);\n".into(),
        ),
    }));
    reduce(ThreadItem::CommandExecution {
        sandbox_type: None,
        model_context: None,
        id: "call-status".into(),
        plugin_id: None,
        script_path: None,
        command: "git status".into(),
        cwd: cwd.clone().into(),
        process_id: None,
        source: CommandExecutionSource::Agent,
        status: codex_app_server_protocol::CommandExecutionStatus::Completed,
        command_actions: Vec::new(),
        aggregated_output: Some("clean\n".into()),
        exit_code: Some(0),
        duration_ms: Some(0),
    });
    reduce(fact(
        "end",
        CodeModeEvent::Finished {
            status: CodeModeStatus::Completed,
        },
    ))
}

fn step(
    response: &str,
    calls: &[&str],
    messages: &[&str],
    admission: Admission,
) -> StepAdmissionItem {
    StepAdmissionItem {
        id: format!("step:{response}"),
        response_id: response.into(),
        completed_at_ms: CLOCK_MS,
        read_tokens: 20_000,
        thinking_tokens: Some(120),
        output_tokens: 34,
        call_ids: calls.iter().map(|call| (*call).to_string()).collect(),
        message_ids: messages.iter().map(|id| (*id).to_string()).collect(),
        admission,
    }
}

#[test]
fn steps_fold_under_one_header_with_their_figures() {
    let mut exec = new_active_exec_command(
        "call-1".into(),
        [
            "cargo",
            "test",
            "-p",
            "codex-tui",
            "--lib",
            "transcript_view::feed",
        ]
        .map(String::from)
        .to_vec(),
        Vec::new(),
        CommandExecutionSource::Agent,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    exec.complete_call(
        "call-1",
        // Items unfold by default, so a header-only activity once dumped all of its output here.
        CommandOutput::new(
            /*exit_code*/ 1,
            "test parse ... FAILED\ntest result: FAILED. 0 passed; 1 failed\n".into(),
        ),
        std::time::Duration::ZERO,
    );
    // Cells arrive as the server announces them: each Thought after its response's members.
    let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
        Arc::new(UserHistoryCell {
            prompt_identity: Default::default(),
            message: "Fix the failing test".into(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
            spoken: false,
        }),
        message("msg-1", "I'll run the tests first."),
        Arc::new(exec),
        thought("resp-1"),
        Arc::new(ArrivalCell::new(ArrivalItem {
            placement: None,
            id: "arrival:resp-1:0".into(),
            response_id: "resp-1".into(),
            announced_at_ms: CLOCK_MS,
            kind: ArrivalKind::Injection,
            source: codex_protocol::step_admission::ArrivalSource::Captured {
                blocks: vec!["<environment_context>cwd</environment_context>".into()],
            },
            label: "environment_context".into(),
            text: None,
        })),
        message("msg-2", "Fixed the parser."),
        thought("resp-2"),
        Arc::new(UserHistoryCell {
            prompt_identity: Default::default(),
            message: "Thanks".into(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
            spoken: false,
        }),
        script_block("exec-1"),
        message("msg-4", "No step billed this reply."),
    ];
    let mut view = TranscriptView::default();
    view.record_step_admission(step(
        "resp-1",
        &["call-1"],
        &["msg-1"],
        Admission::Sealed(SealedAdmission {
            self_tokens: 1_188,
            arrivals: vec![Arrival {
                id: "arrival:resp-1:0".into(),
                kind: ArrivalKind::Injection,
                label: "environment_context".into(),
                tokens: 84,
            }],
            released_tokens: 0,
            precision: AdmissionPrecision::Apportioned,
        }),
    ));
    view.record_step_admission(step(
        "resp-2",
        &[],
        &["msg-2"],
        Admission::Sealed(SealedAdmission {
            self_tokens: 0,
            arrivals: vec![Arrival {
                id: "arrival:resp-2:0".into(),
                kind: ArrivalKind::User,
                label: "user".into(),
                tokens: 3,
            }],
            released_tokens: 0,
            precision: AdmissionPrecision::Exact,
        }),
    ));
    view.record_step_admission(step("resp-3", &["exec-1"], &[], Admission::Pending));
    view.seat_thoughts(&mut cells);
    let clock = crate::history_cell::HistoryTimestamp::from_millis(CLOCK_MS)
        .unwrap()
        .span()
        .content
        .trim()
        .to_owned();
    let mut snapshots = Vec::new();
    let mut screen = |view: &mut TranscriptView, label: &str| {
        let buffer = crate::transcript_view::tests::render(
            view, &cells, /*width*/ 104, /*height*/ 34,
        );
        let text = crate::transcript_view::tests::text(&buffer).replace(&clock, "HH:MM");
        snapshots.push(format!("{label}\n{text}"));
        text
    };
    let click = |view: &mut TranscriptView, text: &str, needle: &str, column: Option<u16>| {
        let (row, line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is drawn:\n{text}"));
        let column = column.unwrap_or_else(|| {
            let start = line.find(needle).unwrap_or_default();
            unicode_width::UnicodeWidthStr::width(&line[..start]) as u16 + 1
        });
        assert!(view.toggle_disclosure_at(&cells, column, row as u16));
    };
    let fold = Some(crate::transcript_geometry::CONTROL_COLUMN + 1);
    let unfolded = screen(&mut view, "unfolded");
    click(&mut view, &unfolded, "[js]", /*column*/ None);
    let script = screen(&mut view, "script");
    // The lower header first, so folding it leaves the upper one's row where it was.
    click(&mut view, &script, "Code Mode", fold);
    click(&mut view, &script, "cargo test", fold);
    screen(&mut view, "folded");
    insta::assert_snapshot!(snapshots.join("\n\n"));

    // A parent's fold must not stand in for consent to reveal retained instructions. The two
    // identities also survive reflow and a new immutable revision of the same semantic item.
    let raw = "<instructions>\n  Be exact.\n</instructions>\n<tools>exec</tools>\n<context>needle Ω</context>";
    let preamble = |id: &str, raw: &str| -> Arc<dyn HistoryCell> {
        Arc::new(PreambleCell::from_item(ThreadItem::Preamble {
            id: id.into(),
            response_id: id.into(),
            completed_at_ms: CLOCK_MS,
            parts: vec![
                codex_protocol::PreamblePart {
                    section: codex_protocol::PreambleSection::Instructions,
                    group: None,
                    label: "Base instructions".into(),
                    tokens: codex_protocol::CensusCount::Counted(700),
                },
                codex_protocol::PreamblePart {
                    section: codex_protocol::PreambleSection::Context,
                    group: None,
                    label: "AGENTS.md /repo".into(),
                    tokens: codex_protocol::CensusCount::Counted(300),
                },
            ],
            raw: None,
            sources: vec![codex_protocol::ContextSource::Prompt {
                label: "instructions".into(),
                text: raw.into(),
            }],
            change: None,
            reported_input_tokens: None,
            window_id: None,
        }))
    };
    let mut cells = vec![
        preamble("window-1", raw),
        preamble("window-2", "other window"),
    ];
    assert_eq!(
        cells[0]
            .raw_lines()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        raw,
    );
    let mut view = TranscriptView::default();
    let mut snapshots = Vec::new();
    let mut screen =
        |view: &mut TranscriptView, cells: &[Arc<dyn HistoryCell>], label: &str, width| {
            let buffer =
                crate::transcript_view::tests::render(view, cells, width, /*height*/ 24);
            let text = crate::transcript_view::tests::text(&buffer).replace(&clock, "HH:MM");
            snapshots.push(format!("{label}\n{}", text.trim_end()));
            text
        };
    let collapsed = screen(&mut view, &cells, "fresh", 80);
    assert_eq!(
        collapsed
            .lines()
            .filter(|line| line.contains("Preamble"))
            .count(),
        2
    );
    assert!(!collapsed.contains("Base instructions"));
    let parent = view.current_layout(&cells, 0).unwrap().fold_targets()[0].clone();
    assert!(view.focus_fold(&cells, 0, parent.clone()));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    let summary = screen(&mut view, &cells, "summary", 80);
    assert!(summary.contains("Base instructions"));
    assert!(summary.contains("Raw Context"));
    assert!(!summary.contains("<instructions>"));
    for _ in 0..2 {
        view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    }
    assert!(!screen(&mut view, &cells, "summary reopened", 80).contains("<instructions>"));
    view.handle_key(crossterm::event::KeyCode::Down.into(), &cells);
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    assert!(screen(&mut view, &cells, "raw explicitly opened", 80).contains("<instructions>"));
    let second_parent = view.current_layout(&cells, 1).unwrap().fold_targets()[0].clone();
    assert!(view.focus_fold(&cells, 1, second_parent));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    assert!(
        !view
            .current_layout(&cells, 1)
            .unwrap()
            .text()
            .contains("other window")
    );
    screen(&mut view, &cells, "narrow raw", 40);
    assert!(screen(&mut view, &cells, "wide raw", 80).contains("<instructions>"));
    assert!(view.focus_fold(&cells, 0, parent.clone()));
    for _ in 0..2 {
        view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    }
    assert!(
        screen(&mut view, &cells, "raw retained across parent fold", 80).contains("<instructions>")
    );
    cells[0] = preamble("window-1", raw);
    view.jump_to_latest();
    assert!(screen(&mut view, &cells, "same item replayed", 80).contains("<instructions>"));
    assert!(view.focus_fold(&cells, 0, parent));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    let manual = view.disclosure.expanded.clone();
    view.begin_search();
    view.paste_search("needle Ω");
    assert!((0..512).any(|_| !view.advance_search(&cells)));
    assert!(
        view.layout(&cells, 0)
            .unwrap()
            .text()
            .contains("<context>needle Ω</context>")
    );
    assert!(screen(&mut view, &cells, "search raw match", 80).contains("needle Ω"));
    view.cancel_search();
    assert_eq!(view.disclosure.expanded, manual);
    assert!(!screen(&mut view, &cells, "search cancelled", 80).contains("<instructions>"));
    insta::assert_snapshot!("preamble_disclosure", snapshots.join("\n\n"));
}
