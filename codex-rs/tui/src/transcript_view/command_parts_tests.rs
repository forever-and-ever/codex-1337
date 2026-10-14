use super::*;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::new_active_exec_command;
use crate::history_cell::ToolOutputCell;
use crate::history_cell::ToolOutputGroup;
use crate::history_cell::activity_parts::FoldTarget;
use crate::history_cell::invocation::Action;
use crate::history_cell::invocation::Panel;
use crate::history_cell::invocation::Port;
use crate::history_cell::output_provenance::OutputMode;
use codex_app_server_protocol::CommandExecutionSource;
use codex_protocol::ToolName;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::tool_input::ToolInput;
use codex_protocol::tool_input::ToolInputPayload;
use codex_shell_command::parse_command::parse_command;
use crossterm::event::KeyCode;
use pretty_assertions::assert_eq;
use std::time::Duration;

#[test]
fn invocation_ports_preserve_sources_across_completion_and_reflow() {
    // Separate script/argument/result folds once disagreed across these transitions.
    let script = "# @exec: {\"max_output_tokens\": 3000}\nsed -n '10,20p' src/main.rs; rg 'TODO' src; echo 'not;a&&boundary'";
    let command = vec!["bash".into(), "-lc".into(), script.into()];
    let mut cell = new_active_exec_command(
        "train".into(),
        command.clone(),
        parse_command(&command),
        CommandExecutionSource::UnifiedExecStartup,
        None,
        false,
    );
    let input = ToolInput {
        id: "input:train".into(),
        call_id: "train".into(),
        name: ToolName::plain("bash"),
        payload: ToolInputPayload::Text(script.into()),
    };
    let mut view = TranscriptView::default();
    view.record_tool_input(input);
    view.sync_live_activity(&[], cell.activity_ids());
    assert!(
        view.sync_live_invocation_tail(80, None, &cell, vec!["live auxiliary".into()])
            .is_some()
    );
    let live = tests::text(&tests::render(&mut view, &[], 80, 26));
    assert!(live.contains("live auxiliary"));
    view.sync_live_tail(80, None, |_| None);
    let mut cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(cell.clone())];
    let mut snapshots = Vec::new();
    let mut render =
        |view: &mut TranscriptView, cells: &[Arc<dyn HistoryCell>], width, label: &str| {
            view.jump_to_latest();
            let buffer = tests::render(view, cells, width, 26);
            let text = tests::text(&buffer);
            snapshots.push(format!("{label}\n{}", text.trim_end()));
            view.current_layout(cells, 0).unwrap()
        };
    let initial = render(&mut view, &cells, 80, "running input");
    assert!(
        initial.text().contains("Search"),
        "initial source: {:?}",
        initial.text()
    );
    assert!(!initial.fold_targets().contains(&FoldTarget::Activity));
    let target = |action| FoldTarget::Invocation {
        call_id: "train".into(),
        action,
    };
    let select = |view: &mut TranscriptView, cells: &[Arc<dyn HistoryCell>], action| {
        assert!(view.focus_fold(cells, 0, target(action)));
        view.handle_key(KeyCode::Enter.into(), cells);
    };
    select(
        &mut view,
        &cells,
        Action::Format(Port::Input, OutputMode::Raw),
    );
    let raw = render(&mut view, &cells, 80, "raw source");
    assert!(raw.text().contains(script));
    let out_position = raw
        .fold_position(&target(Action::Toggle(
            Port::Output,
            Panel::Visible(OutputMode::Fmt),
        )))
        .unwrap();
    cell.complete_call(
        "train",
        CommandOutput::new(0, "combined output".into()),
        Duration::ZERO,
    );
    let returned = r#"{"n":1e+02,"n":2,"output":"combined output\nlast"}"#;
    let receipt = ToolOutputCell::from_output(
        "returned-train",
        Some("train"),
        "bash",
        None,
        &FunctionCallOutputBody::Text(returned.into()),
    )
    .unwrap();
    cells[0] =
        ToolOutputGroup::from_cells([Arc::new(cell) as Arc<dyn HistoryCell>, Arc::new(receipt)]);
    let completed = render(&mut view, &cells, 80, "completed, output hidden");
    assert!(completed.text().contains(script));
    assert!(!completed.text().contains("combined output"));
    select(
        &mut view,
        &cells,
        Action::Toggle(Port::Output, Panel::Visible(OutputMode::Fmt)),
    );
    let both = render(&mut view, &cells, 80, "both ports");
    assert!(both.text().contains(script));
    assert!(both.text().contains("1e+02"));
    assert!(both.text().contains("combined output\nlast"));
    assert_eq!(
        both.fold_position(&target(Action::Toggle(
            Port::Output,
            Panel::Hidden(OutputMode::Fmt)
        )))
        .unwrap()
        .1,
        out_position.1
    );
    select(
        &mut view,
        &cells,
        Action::Toggle(Port::Input, Panel::Hidden(OutputMode::Raw)),
    );
    for width in [40, 104, 80] {
        let layout = render(&mut view, &cells, width, "output only, reflowed");
        assert!(!layout.text().contains(script));
        assert!(layout.text().contains("combined output"));
        for control in layout.fold_targets() {
            let (row, columns, _) = layout.fold_position(&control).unwrap();
            if !columns.is_empty() {
                assert_eq!(layout.fold_at(row, columns.start), Some(control));
            }
        }
    }
    select(
        &mut view,
        &cells,
        Action::Format(Port::Output, OutputMode::Raw),
    );
    let raw = render(&mut view, &cells, 80, "raw output");
    assert!(raw.text().contains(returned));
    select(
        &mut view,
        &cells,
        Action::Toggle(Port::Output, Panel::Hidden(OutputMode::Raw)),
    );
    let collapsed = render(&mut view, &cells, 104, "neither port");
    assert!(collapsed.text().is_empty());
    assert!(
        collapsed
            .fold_targets()
            .iter()
            .all(|control| matches!(control, FoldTarget::Invocation { .. }))
    );
    // The batch wrapper is structural; each child owns its ports. Removing its siblings
    // must not reinterpret an explicit child choice or introduce an aggregate output port.
    let mut view = TranscriptView::default();
    let mut batch: Vec<Arc<dyn HistoryCell>> = Vec::new();
    for (id, name, source, output, tokens) in [
        ("a", "bash", "echo alpha", "alpha result", 31),
        ("b", "python", "print('beta')", "beta result", 79),
        ("c", "fence", "{}", "", 0),
    ] {
        view.record_tool_output_tokens(
            format!("tool-output-tokens:output:{id}"),
            id.into(),
            Some(tokens),
        );
        view.record_tool_input(ToolInput {
            id: format!("input:{id}"),
            call_id: id.into(),
            name: ToolName::plain(name),
            payload: if name == "fence" {
                ToolInputPayload::Json(source.into())
            } else {
                ToolInputPayload::Text(source.into())
            },
        });
        batch.push(Arc::new(
            ToolOutputCell::from_output(
                &format!("output:{id}"),
                Some(id),
                name,
                None,
                &FunctionCallOutputBody::Text(output.into()),
            )
            .unwrap(),
        ));
    }
    view.record_step_admission(codex_protocol::step_admission::StepAdmissionItem {
        id: "step:batch".into(),
        response_id: "batch".into(),
        completed_at_ms: 0,
        read_tokens: 12,
        thinking_tokens: Some(2),
        output_tokens: 8,
        call_ids: vec!["a".into(), "b".into(), "c".into()],
        message_ids: Vec::new(),
        admission: codex_protocol::step_admission::Admission::Pending,
    });
    let header = render(&mut view, &batch, 104, "compound batch");
    let outer = header
        .fold_targets()
        .into_iter()
        .find(|target| matches!(target, FoldTarget::Part(_)))
        .unwrap();
    let shown = tests::text(&tests::render(&mut view, &batch, 104, 26));
    assert!(shown.contains("Command"));
    assert!(shown.contains("Script (bash)"));
    assert!(shown.contains("Script (python)"));
    assert!(shown.contains("Fence"));
    assert!(
        shown
            .lines()
            .any(|line| line.contains("Script (bash)") && line.trim_end().ends_with("31"))
    );
    assert!(
        shown
            .lines()
            .any(|line| line.contains("Script (python)") && line.trim_end().ends_with("79"))
    );
    assert!(!shown.contains("alpha result"));
    assert!(!shown.contains("print('beta')"));
    let input_b = FoldTarget::Invocation {
        call_id: "b".into(),
        action: Action::Toggle(Port::Input, Panel::Visible(OutputMode::Raw)),
    };
    assert!(view.focus_fold(&batch, 1, input_b.clone()));
    view.handle_key(KeyCode::Enter.into(), &batch);
    render(&mut view, &batch, 104, "one child input");
    assert!(tests::text(&tests::render(&mut view, &batch, 104, 26)).contains("print('beta')"));
    for _ in 0..2 {
        assert!(view.focus_fold(&batch, 0, outer.clone()));
        view.handle_key(KeyCode::Enter.into(), &batch);
        render(&mut view, &batch, 104, "outer fold");
    }
    assert!(tests::text(&tests::render(&mut view, &batch, 104, 26)).contains("print('beta')"));
    let singleton = vec![Arc::clone(&batch[1])];
    let one = render(&mut view, &singleton, 104, "regrouped singleton");
    assert!(one.text().contains("print('beta')"));
    assert!(
        one.fold_targets()
            .iter()
            .all(|target| matches!(target, FoldTarget::Invocation { .. }))
    );
    insta::assert_snapshot!(snapshots.join("\n\n"));
    // Source readiness belongs to the invocation, not its changing nested tool set.
    // Retained source-only records and a live call's first paint must agree.
    let script = "const answer = 42;\ntext(\"emitted text\");";
    let owner = codex_app_server_protocol::CodeModeOwner {
        thread_id: codex_protocol::ThreadId::from_u128(7),
        exec_call_id: "code".into(),
        cell_id: None,
        started_at_ms: 0,
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
                false,
                &|_| crate::multi_agents::AgentMetadata::default(),
            )
            .unwrap()
            .block
    };
    let block = reduce(codex_app_server_protocol::ThreadItem::CodeMode(
        codex_app_server_protocol::CodeModeItem {
            id: "code-start".into(),
            exec_call_id: "code".into(),
            cell_id: None,
            created_at: 0,
            event: codex_app_server_protocol::CodeModeEvent::Started {
                call_id: "code".into(),
                script: Some(script.into()),
            },
        },
    ));
    let mut view = TranscriptView::default();
    view.record_step_admission(codex_protocol::step_admission::StepAdmissionItem {
        id: "step:code".into(),
        response_id: "code-response".into(),
        completed_at_ms: 0,
        read_tokens: 0,
        thinking_tokens: None,
        output_tokens: 0,
        call_ids: vec!["code".into()],
        message_ids: Vec::new(),
        admission: codex_protocol::step_admission::Admission::Pending,
    });
    let mut code = vec![block];
    let buffer = tests::render(&mut view, &code, 104, 26);
    let shown = tests::text(&buffer);
    let header_column = shown
        .lines()
        .find(|line| line.contains("Code Mode"))
        .unwrap()
        .find("Code Mode")
        .unwrap();
    let (source_row, source_line) = shown
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("const answer"))
        .unwrap();
    let source_column = source_line.find("const answer").unwrap();
    assert_eq!(source_column, header_column + 2);
    assert_ne!(
        buffer[(source_column as u16, source_row as u16)].fg,
        buffer[((source_column + 15) as u16, source_row as u16)].fg,
    );
    let first = view.current_layout(&code, 0).unwrap();
    assert_eq!(first.text(), script);
    let format = FoldTarget::Invocation {
        call_id: "code".into(),
        action: Action::Format(Port::Input, OutputMode::Fmt),
    };
    assert!(first.fold_position(&format).is_some());
    code[0] = reduce(codex_app_server_protocol::ThreadItem::CommandExecution {
        sandbox_type: None,
        model_context: None,
        id: "child".into(),
        plugin_id: None,
        script_path: None,
        command: "echo child".into(),
        cwd: cwd.clone().into(),
        process_id: None,
        source: CommandExecutionSource::Agent,
        status: codex_app_server_protocol::CommandExecutionStatus::Completed,
        command_actions: Vec::new(),
        aggregated_output: Some("child output".into()),
        exit_code: Some(0),
        duration_ms: Some(0),
    });
    tests::render(&mut view, &code, 104, 26);
    let nested = view.current_layout(&code, 0).unwrap();
    assert_eq!(nested.text(), first.text());
    assert!(nested.fold_position(&format).is_some());
    code[0] = reduce(codex_app_server_protocol::ThreadItem::FunctionCallOutput {
        id: "code-output".into(),
        call_id: Some("code".into()),
        name: "exec".into(),
        namespace: None,
        output: FunctionCallOutputBody::Text("plain result, not code".into()),
    });
    tests::render(&mut view, &code, 104, 26);
    let output = FoldTarget::Invocation {
        call_id: "code".into(),
        action: Action::Toggle(Port::Output, Panel::Visible(OutputMode::Fmt)),
    };
    assert!(view.focus_fold(&code, 0, output));
    view.handle_key(KeyCode::Enter.into(), &code);
    let raw = FoldTarget::Invocation {
        call_id: "code".into(),
        action: Action::Format(Port::Input, OutputMode::Raw),
    };
    assert!(view.focus_fold(&code, 0, raw.clone()));
    view.handle_key(KeyCode::Enter.into(), &code);
    let outer = nested
        .fold_targets()
        .into_iter()
        .find(|target| matches!(target, FoldTarget::Part(_)))
        .unwrap();
    for width in [104, 40, 80] {
        assert!(view.focus_fold(&code, 0, outer.clone()));
        view.handle_key(KeyCode::Enter.into(), &code);
        tests::render(&mut view, &code, width, 26);
        let collapsed = view.current_layout(&code, 0).unwrap();
        assert!(collapsed.text().is_empty());
        assert_eq!(collapsed.row_count(), 1);
        let (row, columns, _) = collapsed.fold_position(&outer).unwrap();
        assert_eq!(collapsed.fold_at(row, columns.start), Some(outer.clone()));
        let screen_row = view
            .visible
            .iter()
            .position(|visible| {
                visible.row == row
                    && visible.layout.fold_at(row, columns.start) == Some(outer.clone())
            })
            .unwrap();
        assert!(view.toggle_disclosure_at(
            &code,
            columns.start + view.area.x,
            screen_row as u16 + view.area.y
        ));
        tests::render(&mut view, &code, width, 26);
        let opened = view.current_layout(&code, 0).unwrap();
        assert!(opened.text().contains(script));
        assert!(opened.text().contains("plain result, not code"));
        assert!(opened.fold_position(&raw).is_some());
        assert!(
            opened
                .fold_position(&FoldTarget::Invocation {
                    call_id: "code".into(),
                    action: Action::Toggle(Port::Output, Panel::Hidden(OutputMode::Fmt)),
                })
                .is_some()
        );
    }

    // Known Python source is code; an arbitrary tool's text payload is not.
    let input = |name| ToolInput {
        id: "input:source".into(),
        call_id: "source".into(),
        name: ToolName::plain(name),
        payload: ToolInputPayload::Text("print(42)".into()),
    };
    let output = ToolOutputCell::from_output(
        "source-output",
        Some("source"),
        "python",
        None,
        &FunctionCallOutputBody::Text("plain result, not Python".into()),
    )
    .unwrap();
    let call = crate::history_cell::invocation::collect(&output).remove(0);
    view.record_tool_input(input("python"));
    let python = view
        .invocations
        .body(&call, true, 80, false, &Default::default());
    assert_eq!(
        python
            .lines
            .iter()
            .map(|line| line.line.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        "print(42)"
    );
    let available = view.invocations.header(&call, true, 80, None, false);
    let mut opaque = TranscriptView::default();
    opaque.record_tool_input(input("opaque"));
    opaque.invocations.apply(
        "source",
        Action::Toggle(Port::Input, Panel::Visible(OutputMode::Raw)),
    );
    let reserved = opaque.invocations.header(&call, true, 80, None, false);
    assert!(
        reserved
            .lines
            .iter()
            .any(|line| line.to_string().contains("[raw]      ]"))
    );
    assert!(!reserved.controls.iter().any(|control| matches!(
        control.target,
        FoldTarget::Invocation {
            action: Action::Format(Port::Input, OutputMode::Fmt),
            ..
        }
    )));
    assert_eq!(
        reserved
            .controls
            .iter()
            .find(|control| matches!(
                control.target,
                FoldTarget::Invocation {
                    action: Action::Toggle(Port::Output, _),
                    ..
                }
            ))
            .unwrap()
            .columns,
        available
            .controls
            .iter()
            .find(|control| matches!(
                control.target,
                FoldTarget::Invocation {
                    action: Action::Toggle(Port::Output, _),
                    ..
                }
            ))
            .unwrap()
            .columns,
    );
}
