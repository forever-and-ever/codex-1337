//! The feed's one interaction worth pinning: steps fold under their headers with their figures,
//! a Code Mode header shows its block's script, and an activity whose only row became its
//! header keeps its details behind its own control. Preamble summaries and retained raw context
//! have independent folds across navigation, reflow, replay, and full-source search.

use super::*;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::new_active_exec_command;
use crate::history_cell::ThoughtTextVisibility;
use crate::history_cell::invocation::Action;
use crate::history_cell::invocation::Panel;
use crate::history_cell::invocation::Port;
use crate::history_cell::output_provenance::OutputMode;
use crate::prompt_order::UserPromptIdentity;
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

/// A finished Code Mode block with optional nested activity and exact returned text.
fn script_block(exec: &str, command: Option<&str>, output: Option<&str>) -> Arc<dyn HistoryCell> {
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
        script: Some(command.map_or_else(
            || "text(\"only emitted text\");".into(),
            |command| format!("const status = await tools.exec_command({{ cmd: {command:?} }});\ntext(status.output);\n"),
        )),
    }));
    if let Some(command) = command {
        reduce(ThreadItem::CommandExecution {
            sandbox_type: None,
            model_context: None,
            id: "call-status".into(),
            plugin_id: None,
            script_path: None,
            command: command.into(),
            cwd: cwd.clone().into(),
            process_id: None,
            source: CommandExecutionSource::Agent,
            status: codex_app_server_protocol::CommandExecutionStatus::Completed,
            command_actions: Vec::new(),
            aggregated_output: Some("clean\n".into()),
            exit_code: Some(0),
            duration_ms: Some(0),
        });
    }
    if let Some(output) = output {
        reduce(ThreadItem::FunctionCallOutput {
            id: format!("{exec}-output"),
            call_id: Some(exec.into()),
            name: "exec".into(),
            namespace: None,
            output: codex_protocol::models::FunctionCallOutputBody::Text(output.into()),
        });
    }
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
            timestamp: None,
            prompt_identity: UserPromptIdentity::default(),
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
            timestamp: None,
            prompt_identity: UserPromptIdentity::default(),
            message: "Thanks".into(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
            spoken: false,
        }),
        script_block("exec-1", Some("git status"), None),
        message("msg-4", "No step billed this reply."),
        Arc::new(crate::history_cell::CompactionCell {
            id: "compact-layout".into(),
            elapsed: Some(std::time::Duration::from_secs(83)),
        }),
        Arc::new(crate::history_cell::AlarmSoundingCell(
            codex_app_server_protocol::alarm_sounding::AlarmSounding {
                id: "check-layout".into(),
                armed_at: CLOCK_MS - 1000,
                sounded_at: CLOCK_MS,
                note: "Check the header alignment".into(),
                cause: codex_app_server_protocol::alarm_sounding::AlarmCause::Clock {
                    late_by_seconds: None,
                },
                still_armed: Vec::new(),
            },
        )),
    ];
    // Restart may sever provider accounting, or replay an arrival before its step.
    // Neither may strand captured text behind a permanently pending token figure.
    let arrival = cells
        .iter()
        .find(|cell| cell.as_any().is::<ArrivalCell>())
        .unwrap();
    let estimated = arrival
        .as_any()
        .downcast_ref::<ArrivalCell>()
        .unwrap()
        .estimated_tokens()
        .unwrap();
    assert!(estimated > 0);
    for admission in [
        None,
        Some(Admission::Pending),
        Some(Admission::Severed),
        Some(Admission::Sealed(SealedAdmission {
            self_tokens: 0,
            arrivals: Vec::new(),
            released_tokens: 0,
            precision: AdmissionPrecision::Exact,
        })),
    ] {
        let mut replay = TranscriptView::default();
        if let Some(admission) = admission {
            replay.record_step_admission(step("resp-1", &[], &[], admission));
        }
        let buffer =
            crate::transcript_view::tests::render(&mut replay, &[Arc::clone(arrival)], 104, 4);
        let rendered = crate::transcript_view::tests::text(&buffer);
        assert!(rendered.contains(&format!("≈{estimated}")), "{rendered}");
    }
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
    let clock = HistoryTimestamp::from_millis(CLOCK_MS)
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
        for (row, line) in text.lines().enumerate() {
            for glyph in ["[+]", "[-]"] {
                for (column, _) in line.match_indices(glyph) {
                    let cell =
                        &buffer[(u16::try_from(column).unwrap(), u16::try_from(row).unwrap())];
                    assert_eq!(cell.fg, ratatui::style::Color::Reset);
                    assert!(!cell.modifier.contains(ratatui::style::Modifier::BOLD));
                }
            }
            for label in [
                "User",
                "Response",
                "Command",
                "Injection",
                "Code Mode",
                "Alarm",
                "Context compacted",
            ] {
                if let Some(column) = line.find(label) {
                    assert_eq!(
                        column,
                        usize::from(crate::transcript_geometry::TITLE_COLUMN),
                        "{line}"
                    );
                    let cell =
                        &buffer[(u16::try_from(column).unwrap(), u16::try_from(row).unwrap())];
                    assert_eq!(
                        cell.fg,
                        match label {
                            "Context compacted" => ratatui::style::Color::Yellow,
                            _ => crate::render::header::style().fg.unwrap(),
                        }
                    );
                    assert!(cell.modifier.contains(ratatui::style::Modifier::BOLD));
                    assert!(!cell.modifier.contains(ratatui::style::Modifier::DIM));
                }
            }
            for label in ["Stored execution", "Check the header alignment"] {
                if let Some(column) = line.find(label) {
                    assert_eq!(
                        column,
                        usize::from(crate::transcript_geometry::GUTTER_COLUMNS),
                        "{line}"
                    );
                }
            }
        }
        snapshots.push(format!("{label}\n{text}"));
        text
    };
    let fresh = screen(&mut view, "unfolded");
    assert!(fresh.contains("[+] Injection"));
    assert!(
        fresh
            .lines()
            .find(|line| line.contains("Injection"))
            .unwrap()
            .contains("≈84")
    );
    let compacted = cells
        .iter()
        .position(|cell| cell.as_any().is::<crate::history_cell::CompactionCell>())
        .unwrap();
    assert!(
        view.current_layout(&cells, compacted)
            .unwrap()
            .fold_targets()
            .is_empty()
    );
    assert!(!fresh.contains("<environment_context>"));
    let injection = cells
        .iter()
        .position(|cell| cell.as_any().is::<ArrivalCell>())
        .unwrap();
    let fold = view
        .current_layout(&cells, injection)
        .unwrap()
        .fold_targets()[0]
        .clone();
    assert!(view.focus_fold(&cells, injection, fold.clone()));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    let opened = screen(&mut view, "injection opened");
    assert!(opened.contains("<environment_context>"));
    assert_eq!(
        opened
            .lines()
            .find(|line| line.trim_start().starts_with("Context"))
            .unwrap()
            .find("Context"),
        Some(usize::from(crate::transcript_geometry::GUTTER_COLUMNS))
    );
    assert!(view.focus_fold(&cells, injection, fold));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    let code = cells
        .iter()
        .position(|cell| cell.as_any().is::<CodeModeCell>())
        .unwrap();
    let control = |call_id: &str, action| FoldTarget::Invocation {
        call_id: call_id.into(),
        action,
    };
    assert!(view.focus_fold(
        &cells,
        code,
        control("exec-1", Action::Format(Port::Input, OutputMode::Raw))
    ));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    screen(&mut view, "script");
    assert!(view.focus_fold(
        &cells,
        code,
        control(
            "exec-1",
            Action::Toggle(Port::Input, Panel::Hidden(OutputMode::Raw))
        )
    ));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    let command = (0..cells.len())
        .find(|index| {
            view.current_layout(&cells, *index).is_some_and(|layout| {
                layout
                    .fold_position(&control(
                        "call-1",
                        Action::Toggle(Port::Input, Panel::Hidden(OutputMode::Fmt)),
                    ))
                    .is_some()
            })
        })
        .unwrap();
    assert!(
        view.focus_fold(
            &cells,
            command,
            control(
                "call-1",
                Action::Toggle(Port::Input, Panel::Hidden(OutputMode::Fmt))
            )
        ),
        "command controls: {:?}",
        view.current_layout(&cells, command).unwrap().fold_targets()
    );
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    screen(&mut view, "folded");
    let manual = view.disclosure.expanded.clone();
    view.begin_search();
    view.paste_search("<environment_context>");
    assert!((0..512).any(|_| !view.advance_search(&cells)));
    assert!(
        view.layout(&cells, injection)
            .unwrap()
            .text()
            .contains("<environment_context>")
    );
    view.cancel_search();
    assert_eq!(view.disclosure.expanded, manual);
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
    // Source formatting cannot depend on nested tool members: a source-only call must
    // paint its settled highlighted input immediately, live and after admission.
    let exec = "source-only";
    let mut cells = vec![script_block(
        exec,
        None,
        Some(r#"{"message":"only emitted text"}"#),
    )];
    let mut view = TranscriptView::default();
    view.record_step_admission(step("source-step", &[exec], &[], Admission::Pending));
    // A separately cached legacy live-search projection once omitted visible Code Mode source.
    let mut live_view = TranscriptView::default();
    live_view.sync_live_activity(&[], cells[0].activity_ids());
    live_view.sync_live_invocation_tail(80, None, cells[0].as_ref(), Vec::new());
    crate::transcript_view::tests::render(&mut live_view, &[], 80, 20);
    live_view.begin_search();
    live_view.paste_search("text(");
    live_view.sync_search_invocation_tail(80, None, cells[0].as_ref(), Vec::new());
    assert!((0..512).any(|_| !live_view.advance_search(&[])));
    assert!(live_view.is_search_expanded(EntryKey::Live));
    live_view.cancel_search();
    let target = |action| FoldTarget::Invocation {
        call_id: exec.into(),
        action,
    };
    let buffer = crate::transcript_view::tests::render(&mut view, &cells, 104, 20);
    let shown = crate::transcript_view::tests::text(&buffer);
    assert!(!shown.contains("text("));
    let (row, header) = shown
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("Code Mode"))
        .unwrap();
    for label in ["[in", "[raw]", "[out"] {
        let column = header.find(label).unwrap() + 1;
        assert!(
            !buffer[(u16::try_from(column).unwrap(), u16::try_from(row).unwrap())]
                .modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
    }
    let layout = view.current_layout(&cells, 0).unwrap();
    assert!(
        !layout
            .fold_targets()
            .contains(&target(Action::Format(Port::Input, OutputMode::Fmt)))
    );
    assert!(
        layout
            .fold_targets()
            .iter()
            .all(|target| !matches!(target, FoldTarget::Part(_)))
    );
    assert!(view.focus_fold(
        &cells,
        0,
        target(Action::Format(Port::Input, OutputMode::Raw)),
    ));
    let _ = view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    drop(crate::transcript_view::tests::render(
        &mut view, &cells, 104, 20,
    ));
    assert!(view.focus_fold(
        &cells,
        0,
        target(Action::Toggle(
            Port::Output,
            Panel::Visible(OutputMode::Fmt)
        ))
    ));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    for width in [104, 40, 80] {
        crate::transcript_view::tests::render(&mut view, &cells, width, 24);
        let layout = view.current_layout(&cells, 0).unwrap();
        assert!(layout.text().contains("text(\"only emitted text\");"));
        assert!(
            layout
                .text()
                .contains("{\n  \"message\": \"only emitted text\"\n}")
        );
        for mode in [OutputMode::Raw, OutputMode::Fmt] {
            let control = target(Action::Format(Port::Output, mode));
            assert!(view.focus_fold(&cells, 0, control.clone()));
            view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
            crate::transcript_view::tests::render(&mut view, &cells, width, 24);
            assert!(
                view.current_layout(&cells, 0)
                    .unwrap()
                    .fold_position(&control)
                    .is_some()
            );
        }
    }
    cells[0] = script_block(exec, None, Some(""));
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (220, 220, 220),
            bg: (20, 20, 20),
        },
        || {
            for width in [104, 40, 80] {
                let buffer = crate::transcript_view::tests::render(&mut view, &cells, width, 24);
                let shown = crate::transcript_view::tests::text(&buffer);
                let (row, line) = shown
                    .lines()
                    .enumerate()
                    .find(|(_, line)| line.contains("text("))
                    .unwrap();
                let column = line.find("text(").unwrap();
                assert_eq!(
                    buffer[(u16::try_from(column).unwrap(), u16::try_from(row).unwrap())].bg,
                    crate::style::context_message_style().bg.unwrap()
                );
                assert!(
                    view.current_layout(&cells, 0)
                        .unwrap()
                        .text()
                        .contains("only emitted text")
                );
            }
        },
    );
}
