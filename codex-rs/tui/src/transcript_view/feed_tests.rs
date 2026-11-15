//! The feed's one interaction worth pinning: steps fold under their headers with their figures,
//! a Code Mode header shows its block's script, and an activity whose only row became its
//! header keeps its details behind its own control. Preamble summaries and retained raw context
//! have independent folds across navigation, reflow, replay, and full-source search.

use super::*;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::new_active_exec_command;
use crate::history_cell::UserMessagePresentation;
use crate::history_cell::activity_parts::FoldTarget;
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
use codex_app_server_protocol::ThreadItemPayload;
use codex_protocol::ThreadId;
use codex_protocol::step_admission::Admission;
use codex_protocol::step_admission::AdmissionPrecision;
use codex_protocol::step_admission::Arrival;
use codex_protocol::step_admission::ArrivalKind;
use codex_protocol::step_admission::SealedAdmission;

const CLOCK_MS: i64 = 1_700_000_000_000;

fn thought(response: &str) -> Arc<dyn HistoryCell> {
    Arc::new(ThoughtCell::from_item(
        codex_app_server_protocol::ThreadItem {
            payload: ThreadItemPayload::Thought {
                id: format!("thought:{response}"),
                response_id: response.into(),
                summary: Vec::new(),
                content: vec!["Considering the failure".into()],
                reported_reasoning_tokens: None,
            },
            started_at_ms: None,
            completed_at_ms: Some(CLOCK_MS),
        },
        std::path::Path::new("/tmp"),
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
        ThreadItemPayload::CodeMode(CodeModeItem {
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
                crate::history_cell::ImagePathOrigin::Local,
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
        reduce(ThreadItemPayload::CommandExecution {
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
        reduce(ThreadItemPayload::FunctionCallOutput {
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
) -> codex_app_server_protocol::ThreadItem {
    codex_app_server_protocol::ThreadItem {
        payload: ThreadItemPayload::StepAdmission(StepAdmissionItem {
            id: format!("step:{response}"),
            response_id: response.into(),
            read_tokens: 20_000,
            thinking_tokens: Some(120),
            output_tokens: 34,
            call_ids: calls.iter().map(|call| (*call).to_string()).collect(),
            message_ids: messages.iter().map(|id| (*id).to_string()).collect(),
            admission,
        }),
        started_at_ms: None,
        completed_at_ms: Some(CLOCK_MS),
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
            presentation: UserMessagePresentation::default(),
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
            presentation: UserMessagePresentation::default(),
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
            for glyph in [false, true].map(|open| crate::render::disclosure::glyph(open).trim()) {
                for (offset, _) in line.match_indices(glyph) {
                    let column = unicode_width::UnicodeWidthStr::width(&line[..offset]);
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
                if let Some(offset) = line.find(label) {
                    let column = unicode_width::UnicodeWidthStr::width(&line[..offset]);
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
                            "User" => crate::warpaint::Bracket::Prompt.color(),
                            "Response" => crate::warpaint::Bracket::Response.color(),
                            _ => crate::render::header::style().fg.unwrap(),
                        }
                    );
                    assert!(cell.modifier.contains(ratatui::style::Modifier::BOLD));
                    assert!(!cell.modifier.contains(ratatui::style::Modifier::DIM));
                }
            }
            for label in ["Stored execution", "Check the header alignment"] {
                if let Some(offset) = line.find(label) {
                    let column = unicode_width::UnicodeWidthStr::width(&line[..offset]);
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
    assert!(
        fresh.contains(&format!(
            "{} Injection",
            crate::render::disclosure::glyph(false)
        )),
        "{fresh}"
    );
    assert_eq!(fresh.matches("[cot]").count(), 2);
    assert!(!fresh.contains("Thought") && !fresh.contains("Considering the failure"));
    // Reasoning belongs to its response, not the tool it called. Opening it must not
    // reveal a sibling response or acquire a permanently visible child heading.
    // Narrow port strips wrap independently of reasoning; paint and hit targets must agree.
    for width in [32, 104] {
        for open in [true, false] {
            let rendered = crate::transcript_view::tests::text(
                &crate::transcript_view::tests::render(&mut view, &cells, width, 80),
            );
            let (row, line) = rendered
                .lines()
                .enumerate()
                .find(|(_, line)| line.contains("[cot]"))
                .unwrap();
            let column = line[..line.find("[cot]").unwrap()].chars().count();
            assert!(view.toggle_disclosure_at(
                &cells,
                u16::try_from(column).unwrap(),
                u16::try_from(row).unwrap()
            ));
            assert_eq!(view.disclosure.expanded.contains("thought:resp-1"), open);
        }
    }
    let cot = FoldTarget::Thought(vec!["thought:resp-1".into()]);
    let source = (0..cells.len())
        .find(|index| {
            view.current_layout(&cells, *index)
                .is_some_and(|layout| layout.fold_position(&cot).is_some())
        })
        .unwrap();
    assert!(view.focus_fold(&cells, source, cot.clone()));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
    let opened = screen(&mut view, "reasoning opened");
    assert_eq!(opened.matches("Considering the failure").count(), 1);
    assert!(!opened.contains("Thought"));
    assert!(view.focus_fold(&cells, source, cot));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
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
    assert_eq!(
        view.current_layout(&cells, compacted)
            .unwrap()
            .fold_targets(),
        [] as [FoldTarget; 0]
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
    // The words start right of the bracket's stroke, in the body's column.
    let context = opened
        .lines()
        .find(|line| {
            line.trim_start_matches(|c: char| {
                c.is_whitespace() || ('\u{2500}'..='\u{257f}').contains(&c)
            })
            .starts_with("Context")
        })
        .unwrap();
    assert_eq!(
        unicode_width::UnicodeWidthStr::width(&context[..context.find("Context").unwrap()]),
        usize::from(crate::transcript_geometry::GUTTER_COLUMNS)
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
        Arc::new(PreambleCell::from_item(
            codex_app_server_protocol::ThreadItem {
                payload: ThreadItemPayload::Preamble {
                    id: id.into(),
                    response_id: id.into(),
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
                },
                started_at_ms: None,
                completed_at_ms: Some(CLOCK_MS),
            },
        ))
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
    // A Thought can host another member's header. Repairing a source-only Code Mode
    // header must not promote that host's independently closed reasoning body.
    let mut cells = vec![
        script_block("cot-source-only", None, Some("public result")),
        Arc::new(ThoughtCell::from_item(
            codex_app_server_protocol::ThreadItem {
                payload: ThreadItemPayload::Thought {
                    id: "thought:cot-source-step".into(),
                    response_id: "cot-source-step".into(),
                    summary: vec!["Provider summary".into()],
                    content: vec![
                        "Private alpha.".into(),
                        "Private omega.".into(),
                        "Private terminus.".into(),
                    ],
                    reported_reasoning_tokens: Some(42),
                },
                started_at_ms: None,
                completed_at_ms: Some(CLOCK_MS),
            },
            std::path::Path::new("/tmp"),
        )) as Arc<dyn HistoryCell>,
    ];
    let mut view = TranscriptView::default();
    view.record_step_admission(step(
        "cot-source-step",
        &["cot-source-only"],
        &[],
        Admission::Pending,
    ));
    view.seat_thoughts(&mut cells);
    assert!(cells[0].as_any().is::<ThoughtCell>());
    let cot = FoldTarget::Thought(vec!["thought:cot-source-step".into()]);
    for open in [false, true, false] {
        if view.disclosure.expanded.contains("thought:cot-source-step") != open {
            assert!(view.focus_fold(&cells, 0, cot.clone()));
            view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
        }
        for width in [104, 40, 80] {
            let shown = crate::transcript_view::tests::text(
                &crate::transcript_view::tests::render(&mut view, &cells, width, 24),
            );
            assert_eq!(shown.matches("[cot]").count(), 1, "{shown}");
            assert_eq!(shown.matches("Code Mode").count(), 1, "{shown}");
            assert!(
                !shown.contains(crate::render::disclosure::glyph(false).trim()),
                "{shown}"
            );
            assert!(!shown.contains("Provider summary"), "{shown}");
            for paragraph in ["Private alpha.", "Private omega.", "Private terminus."] {
                assert_eq!(
                    shown.matches(paragraph).count(),
                    usize::from(open),
                    "{shown}"
                );
            }
            let targets = view.current_layout(&cells, 0).unwrap().fold_targets();
            assert_eq!(targets.iter().filter(|target| **target == cot).count(), 1);
            assert!(!targets.contains(&FoldTarget::Activity));
        }
    }

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
                let (row, _) = shown
                    .lines()
                    .enumerate()
                    .find(|(_, line)| line.contains("text("))
                    .unwrap();
                assert_eq!(
                    buffer[(
                        crate::transcript_geometry::CONTROL_COLUMN,
                        u16::try_from(row).unwrap()
                    )]
                        .fg,
                    crate::warpaint::Bracket::Call.color()
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
    // A farm RPC can finish between two calls in the same billed step. Its independent
    // header must break the visual segment without becoming part of the step's fold.
    let cells: Vec<Arc<dyn HistoryCell>> = vec![
        script_block("farm-before", None, Some("before farming")),
        Arc::new(crate::history_cell::FarmReceipt {
            thread_id: ThreadId::from_u128(7),
            request_id: "independent-farm".into(),
            message: "Farmed to /root/farm_1".into(),
        }),
        script_block("farm-after", None, Some("after farming")),
        Arc::new(crate::history_cell::new_info_event(
            "Model interrupted to submit steer instructions.".into(),
            None,
        )),
        Arc::new(crate::history_cell::CompactionCell {
            id: "notification-layout".into(),
            elapsed: None,
        }),
        Arc::new(crate::history_cell::WorkStatusCell::new(
            "Checking notifications".into(),
            None,
        )),
    ];
    let mut view = TranscriptView::default();
    view.record_step_admission(step(
        "farm-step",
        &["farm-before", "farm-after"],
        &[],
        Admission::Pending,
    ));
    for width in [104, 40] {
        let buffer = crate::transcript_view::tests::render(&mut view, &cells, width, 24);
        let shown = crate::transcript_view::tests::text(&buffer);
        for label in [
            "Farmed",
            "Model interrupted",
            "Context compacted",
            "Activity",
        ] {
            let (row, line) = shown
                .lines()
                .enumerate()
                .find(|(_, line)| line.contains(label))
                .unwrap();
            let offset = line.find(label).unwrap();
            let column = unicode_width::UnicodeWidthStr::width(&line[..offset]);
            assert_eq!(
                column,
                usize::from(crate::transcript_geometry::TITLE_COLUMN),
                "{line}"
            );
            if label != "Activity" {
                let row = u16::try_from(row).unwrap();
                let column = u16::try_from(column).unwrap();
                for column in [column - 2, column] {
                    let cell = &buffer[(column, row)];
                    assert_eq!(cell.fg, ratatui::style::Color::Yellow);
                    assert!(cell.modifier.contains(ratatui::style::Modifier::BOLD));
                }
                assert!(line[..offset].ends_with("• "));
            }
        }
        assert_eq!(shown.matches("Code Mode").count(), 2, "{shown}");
        assert_eq!(
            view.current_layout(&cells, 1).unwrap().fold_targets(),
            [] as [FoldTarget; 0]
        );
    }
}

#[test]
fn grouped_folds_follow_history_arrivals_without_changing_other_kinds() {
    use crossterm::event::KeyModifiers;
    use crossterm::event::MouseButton;
    use crossterm::event::MouseEvent;
    use crossterm::event::MouseEventKind;
    fn arrival(id: &str, kind: ArrivalKind) -> Arc<dyn HistoryCell> {
        Arc::new(ArrivalCell::new(ArrivalItem {
            placement: None,
            id: id.into(),
            response_id: format!("response:{id}"),
            announced_at_ms: CLOCK_MS,
            kind,
            source: codex_protocol::step_admission::ArrivalSource::Captured {
                blocks: vec![format!("**body:{id}**")],
            },
            label: id.into(),
            text: None,
        }))
    }
    const MESSAGE: &str = "**body:second**\n\n- first item\n- second item\n\n[notes](/var/log/agent-notes.md)\n\n```rust\nfn report() {\n    println!(\"ready\");\n}\n```";
    let mut cells = vec![
        arrival("first", ArrivalKind::InterAgent),
        Arc::new(crate::history_cell::DelegatedMessageCell::new(
            crate::dynamic_tools::DelegatedMessage {
                source_thread_id: "sender-thread".into(),
                source_thread_name: Some("TEST_AGENT".into()),
                sent_at: Some(CLOCK_MS),
                input: MESSAGE.into(),
            },
        )),
        arrival("injected", ArrivalKind::Injection),
        script_block("code", None, Some("result")),
    ];
    let mut view = TranscriptView::default();
    // Sender attribution must share the ordinary Message header and its fold group,
    // not become an indented replacement header on coordination envelopes.
    let rendered = crate::transcript_view::tests::render(&mut view, &cells, 104, 80);
    let text = crate::transcript_view::tests::text(&rendered);
    let (row, header) = text
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("Sent by"))
        .unwrap();
    let column = |needle: &str| {
        unicode_width::UnicodeWidthStr::width(&header[..header.find(needle).unwrap()])
    };
    assert_eq!(
        column("Message"),
        usize::from(crate::transcript_geometry::TITLE_COLUMN)
    );
    assert!(header.contains("Message  Sent by TEST_AGENT"));
    let sender_column = column("Sent by");
    assert_eq!(
        rendered[(sender_column.try_into().unwrap(), row.try_into().unwrap())].fg,
        crate::warpaint::Figure::InkMuted.color()
    );
    assert!(
        header.contains(
            HistoryTimestamp::from_millis(CLOCK_MS)
                .unwrap()
                .span()
                .content
                .trim()
        )
    );
    assert!(!text.contains("**body:first**"));
    assert!(!text.contains("**body:second**"));
    assert!(text.contains("first item"));
    assert!(text.contains("second item"));
    assert!(text.contains("/var/log/agent-notes.md"));
    assert!(text.contains("fn report()"));
    let message = feed_entry(cells[1].as_ref()).unwrap();
    assert!(matches!(
        message.feed_kind(),
        EntryKind::Arrival(ArrivalKind::InterAgent)
    ));
    assert_eq!(
        cells[1]
            .raw_lines()
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        MESSAGE
    );
    let lines = message
        .feed_body(
            40,
            &FoldState::default(),
            crate::history_cell::output_provenance::OutputMode::Raw,
        )
        .lines;
    assert!(
        lines
            .iter()
            .filter_map(|line| line.source.as_ref())
            .any(|source| {
                source.bracket == Some(crate::warpaint::Bracket::Message)
                    && source.copy.as_ref().is_some_and(|copy| !copy.code)
            })
    );
    assert!(
        lines
            .iter()
            .filter_map(|line| line.source.as_ref())
            .any(|source| {
                source.text.contains("    println!(\"ready\");")
                    && source.copy.as_ref().is_some_and(|copy| copy.code)
            })
    );
    let click =
        |view: &mut TranscriptView, cells: &[Arc<dyn HistoryCell>], index: usize, modifiers| {
            view.jump_to_latest();
            crate::transcript_view::tests::render(view, cells, 104, 80);
            let layout = view.current_layout(cells, index).unwrap();
            let target = layout
                .fold_targets()
                .into_iter()
                .find(|target| matches!(target, FoldTarget::Part(_)))
                .unwrap();
            let (row, columns, _) = layout.fold_position(&target).unwrap();
            let visible = view
                .visible
                .iter()
                .position(|visible| visible.index == index && visible.row == row)
                .unwrap();
            assert!(matches!(
                view.handle_mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: columns.start,
                        row: u16::try_from(visible).expect("viewport row"),
                        modifiers,
                    },
                    cells
                ),
                Some(crate::transcript_view::input::ViewAction::Changed)
            ));
        };
    let body = |view: &mut TranscriptView, cells: &[Arc<dyn HistoryCell>], index| {
        crate::transcript_view::tests::render(view, cells, 104, 80);
        view.current_layout(cells, index).unwrap().text().to_owned()
    };
    click(&mut view, &cells, 0, KeyModifiers::NONE); // Mixed state: first closed, second open.
    click(&mut view, &cells, 0, KeyModifiers::CONTROL); // Open all Messages.
    assert!(body(&mut view, &cells, 1).contains("body:second"));
    assert!(!body(&mut view, &cells, 2).contains("body:injected"));
    click(&mut view, &cells, 1, KeyModifiers::NONE); // Individual exception.
    assert!(!body(&mut view, &cells, 1).contains("body:second"));
    click(&mut view, &cells, 0, KeyModifiers::CONTROL); // Close all; replace exceptions.
    assert!(!body(&mut view, &cells, 0).contains("body:first"));
    cells.push(arrival("new", ArrivalKind::InterAgent));
    cells.insert(0, arrival("older", ArrivalKind::InterAgent));
    assert!(!body(&mut view, &cells, 0).contains("body:older"));
    assert!(!body(&mut view, &cells, cells.len() - 1).contains("body:new"));
    click(&mut view, &cells, 0, KeyModifiers::CONTROL);
    for index in [0, 1, 2, cells.len() - 1] {
        assert!(body(&mut view, &cells, index).contains("body:"));
    }
    assert!(!body(&mut view, &cells, 3).contains("body:injected"));
    assert!(body(&mut view, &cells, 4).contains("Code Mode"));
    let mut fresh = TranscriptView::default();
    assert!(body(&mut fresh, &cells, 1).contains("body:first"));
}

#[test]
fn dialogue_mode_keeps_prompts_and_replies() {
    use crate::history_cell::UserHistoryCell;
    use crate::prompt_order::UserPromptIdentity;

    let user = |text: &str| -> Arc<dyn HistoryCell> {
        Arc::new(UserHistoryCell {
            timestamp: None,
            prompt_identity: UserPromptIdentity::default(),
            message: text.into(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
            presentation: UserMessagePresentation::default(),
        })
    };
    let notice = || -> Arc<dyn HistoryCell> {
        Arc::new(crate::history_cell::NotificationCell::new(
            "notification".into(),
        ))
    };
    let mut cells = vec![
        user("First prompt"),
        message("commentary", "I'll inspect the code."),
        script_block("first-code", None, Some("model output inside code")),
        message("answer", "Final answer"),
        notice(),
        user("Second prompt"),
        script_block("second-code", None, None),
    ];
    let mut view = TranscriptView::default();
    // A completion can beat the prepared message. Neither phase-less commentary nor a
    // message claimed by a tool step may accidentally join (or swallow) the dialogue.
    view.record_step_admission(step(
        "answer-step",
        &["first-code"],
        &["answer"],
        Admission::Sealed(SealedAdmission {
            self_tokens: 100,
            arrivals: Vec::new(),
            released_tokens: 0,
            precision: AdmissionPrecision::Apportioned,
        }),
    ));
    view.record_final_response("answer".into());
    view.live = Some(Arc::new(crate::transcript_view::TextLayout::new(
        vec!["Responding".into()],
        104,
    )));
    let paint = |view: &mut TranscriptView, cells: &[Arc<dyn HistoryCell>]| {
        crate::transcript_view::tests::render(view, cells, 104, 80)
    };
    paint(&mut view, &cells);
    // Hiding the work being read still leaves the dialogue on screen.
    view.jump_to_entry(&cells, 2);
    view.toggle_lens(&cells);
    assert!(view.dialogue_only());
    let rendered = crate::transcript_view::tests::text(&paint(&mut view, &cells));
    let headers = rendered
        .lines()
        .filter(|line| line.contains("Response"))
        .collect::<Vec<_>>();
    assert_eq!(headers.len(), 1, "{rendered}");
    assert!(
        headers[0].contains(
            HistoryTimestamp::from_millis(CLOCK_MS)
                .unwrap()
                .span()
                .content
                .trim()
        ),
        "{rendered}"
    );
    // Prompts and answers stack without the blank rows that set them apart from work.
    assert!(
        !rendered
            .lines()
            .skip_while(|line| !line.contains("First prompt"))
            .take_while(|line| !line.contains("Second prompt"))
            .any(|line| line.trim().is_empty()),
        "{rendered}"
    );
    for index in [1, 2, 4, 6, cells.len()] {
        assert_eq!(view.current_layout(&cells, index).unwrap().row_count(), 0);
    }
    for index in [0, 3, 5] {
        assert!(view.current_layout(&cells, index).unwrap().row_count() > 0);
    }
    // Full-content search/copy can still reach hidden evidence.
    assert!(
        view.entry_layout(&cells, 6, true)
            .unwrap()
            .text()
            .contains("Code Mode")
    );

    // Older pages and later answers join the dialogue by identity, not position.
    cells.splice(
        0..0,
        [user("Older prompt"), script_block("older", None, None)],
    );
    cells.push(notice());
    view.record_final_response("later-answer".into());
    cells.push(message("later-answer", "Later final answer"));
    paint(&mut view, &cells);
    for index in [1, cells.len() - 2] {
        assert_eq!(view.current_layout(&cells, index).unwrap().row_count(), 0);
    }
    assert!(
        view.current_layout(&cells, cells.len() - 1)
            .unwrap()
            .text()
            .contains("Later final")
    );
    // Until a final answer supersedes it, what the agent says after a prompt is its reply.
    cells.push(user("Steer"));
    cells.push(message("progress", "Still digging."));
    paint(&mut view, &cells);
    assert!(
        view.current_layout(&cells, cells.len() - 1)
            .unwrap()
            .text()
            .contains("Still digging")
    );
    view.record_final_response("closing".into());
    cells.push(message("closing", "Dug out."));
    paint(&mut view, &cells);
    assert_eq!(
        view.current_layout(&cells, cells.len() - 2)
            .unwrap()
            .row_count(),
        0
    );
    assert!(
        view.current_layout(&cells, cells.len() - 1)
            .unwrap()
            .text()
            .contains("Dug out")
    );
    view.toggle_lens(&cells);
    assert!(!view.dialogue_only());
    for index in [1, 3, 4, 6, 8, cells.len()] {
        assert!(view.current_layout(&cells, index).unwrap().row_count() > 0);
    }

    // A final answer and its accounting/CoT witness are separate immutable cells,
    // but one feed item: hiding the work must not strand its header.
    for readable in [false, true] {
        let cells = vec![
            user("Question"),
            Arc::new(ThoughtCell::from_item(
                codex_app_server_protocol::ThreadItem {
                    payload: ThreadItemPayload::Thought {
                        id: "thought:final-step".into(),
                        response_id: "final-step".into(),
                        summary: Vec::new(),
                        content: if readable {
                            vec!["Considering the failure".into()]
                        } else {
                            Vec::new()
                        },
                        reported_reasoning_tokens: None,
                    },
                    started_at_ms: None,
                    completed_at_ms: Some(CLOCK_MS),
                },
                std::path::Path::new("/tmp"),
            )),
            message("final", "The completed answer"),
        ];
        let mut view = TranscriptView::default();
        view.record_step_admission(step(
            "final-step",
            &[],
            &["final"],
            Admission::Sealed(SealedAdmission {
                self_tokens: 100,
                arrivals: Vec::new(),
                released_tokens: 0,
                precision: AdmissionPrecision::Apportioned,
            }),
        ));
        view.record_final_response("final".into());
        let before = crate::transcript_view::tests::text(&paint(&mut view, &cells));
        let headers = before
            .lines()
            .filter(|line| line.contains("Response"))
            .collect::<Vec<_>>();
        assert_eq!(headers.len(), 1, "{before}");
        let header = headers[0];
        let clock = HistoryTimestamp::from_millis(CLOCK_MS).unwrap();
        assert!(header.contains(clock.span().content.trim()), "{before}");
        assert!(header.contains("254"), "{before}");

        view.toggle_lens(&cells);
        let after = crate::transcript_view::tests::text(&paint(&mut view, &cells));
        assert_eq!(
            after
                .lines()
                .filter(|line| line.contains("Response"))
                .collect::<Vec<_>>(),
            vec![header]
        );
        assert!(after.contains("The completed answer"), "{after}");
        assert_eq!(after.contains("[cot]"), readable);
        if readable {
            let (row, line) = after
                .lines()
                .enumerate()
                .find(|(_, line)| line.contains("[cot]"))
                .unwrap();
            let column = line[..line.find("[cot]").unwrap()].chars().count();
            assert!(view.toggle_disclosure_at(
                &cells,
                column.try_into().unwrap(),
                row.try_into().unwrap()
            ));
            let expanded = crate::transcript_view::tests::text(&paint(&mut view, &cells));
            assert!(expanded.contains("Considering the failure"), "{expanded}");
        }
    }
}
