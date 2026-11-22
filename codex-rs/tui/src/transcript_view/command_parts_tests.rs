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
use std::collections::HashMap;
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
    view.invocations.apply(
        "train",
        Action::Toggle(Port::Input, Panel::Visible(OutputMode::Fmt)),
    );
    view.sync_live_activity(&[], cell.activity_ids());
    assert!(
        view.sync_live_invocation_tail(80, None, &cell, vec!["live auxiliary".into()])
            .is_some()
    );
    let live = tests::text(&tests::render(&mut view, &[], 80, 26));
    assert!(live.contains("live auxiliary"));
    view.sync_live_tail(80, None, |_| None);
    let mut cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(cell.clone())];
    let assert_title_color = |buffer: &Buffer, title: &str, active| {
        let text = tests::text(buffer);
        let (row, line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(title))
            .unwrap_or_else(|| panic!("missing {title} (active={active}) in {text}"));
        let column = line.find(title).unwrap();
        assert_eq!(line[column..].contains('•'), active, "{line}");
        let expected = if active {
            crate::render::header::work_style()
        } else {
            crate::render::header::style()
        };
        assert_eq!(
            Some(buffer[(u16::try_from(column).unwrap(), u16::try_from(row).unwrap())].fg),
            expected.fg
        );
    };
    let mut snapshots = Vec::new();
    let mut render =
        |view: &mut TranscriptView, cells: &[Arc<dyn HistoryCell>], width, label: &str| {
            for cell in cells {
                // The color-only traversal must agree without building these projections.
                assert_eq!(
                    crate::history_cell::has_active_invocations(cell.as_ref()),
                    crate::history_cell::invocation::collect(cell.as_ref())
                        .iter()
                        .any(crate::history_cell::invocation::Invocation::is_active),
                    "{label}",
                );
            }
            view.jump_to_latest();
            let buffer = tests::render(view, cells, width, 26);
            let text = tests::text(&buffer);
            snapshots.push(format!("{label}\n{}", text.trim_end()));
            view.current_layout(cells, 0).unwrap()
        };
    let initial = render(&mut view, &cells, 80, "running input");
    let retained_calls = crate::history_cell::invocation::collect(cells[0].as_ref());
    let retained_prepared = view.invocations.prepared(
        &retained_calls[0],
        invocation::PreparationSource::Retained(&cells[0]),
    );
    assert_eq!(retained_prepared.work(), [1, 1, 0, 0]);
    // Another call arriving must not evict an unchanged, already-open source pane.
    view.record_tool_input(ToolInput {
        id: "input:unrelated".into(),
        call_id: "unrelated".into(),
        name: ToolName::plain("python"),
        payload: ToolInputPayload::Text("print(1)".into()),
    });
    assert!(std::rc::Rc::ptr_eq(
        &retained_prepared,
        &view.invocations.prepared(
            &retained_calls[0],
            invocation::PreparationSource::Retained(&cells[0]),
        )
    ));
    for _ in 0..3 {
        view.cache.clear();
        tests::render(&mut view, &cells, 80, 26);
    }
    assert_eq!(retained_prepared.work(), [1, 1, 0, 0]);
    assert_title_color(
        &tests::render(&mut view, &cells, 80, 26),
        "Script (bash)",
        true,
    );
    assert!(
        initial.text().contains("Search"),
        "initial source: {:?}",
        initial.text()
    );
    assert!(!initial.fold_targets().contains(&FoldTarget::Activity));
    assert!(!initial.text().contains("not;a&&boundary"));
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
    assert_title_color(
        &tests::render(&mut view, &cells, 80, 26),
        "Script (bash)",
        false,
    );
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
    assert_eq!(collapsed.text(), "Script (bash)");
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
    view.record_step_admission(codex_app_server_protocol::ThreadItem {
        payload: codex_app_server_protocol::ThreadItemPayload::StepAdmission(
            codex_app_server_protocol::StepAdmissionItem {
                id: "step:batch".into(),
                response_id: "batch".into(),
                read_tokens: 12,
                thinking_tokens: Some(2),
                output_tokens: 8,
                call_ids: vec!["a".into(), "b".into(), "c".into()],
                message_ids: Vec::new(),
                admission: codex_protocol::step_admission::Admission::Pending,
            },
        ),
        started_at_ms: None,
        completed_at_ms: None,
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
    assert!(view.focus_fold(&batch, 1, input_b));
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
    // A source-first highlight once bypassed every semantic child projection.
    // Hold the initial input until real call arguments arrive; raw remains exact.
    let script = concat!(
        "const answer = 42;\n",
        "text(await fs.readFile(\"src/main.rs\", \"utf8\"));\n",
        "await fs.writeFile(\"notes.txt\", \"line one\\nline two\");\n",
        "await tools.update_work_status({activity: \"Reading source\"});\n",
        "text(await tools.spawn_agent({message: \"Inspect source\"}));\n",
        "text(await tools.mcp__depot__sem_read_outline({file: \"src/main.rs\"}));\n",
        "text(await tools.exec_command({cmd: \"cat Cargo.toml\"}));\n",
    );
    let owner = codex_app_server_protocol::CodeModeOwner {
        thread_id: codex_protocol::ThreadId::from_u128(7),
        exec_call_id: "code".into(),
        cell_id: None,
        started_at_ms: 0,
    };
    let cwd = codex_utils_absolute_path::AbsolutePathBuf::current_dir().unwrap();
    for admitted in [true, false] {
        let mut reducers = crate::code_mode_transcript::Reducers::default();
        let mut reduce = |item| {
            let owned =
                codex_app_server_protocol::CodeModeOwned::new(owner.clone(), Box::new(item));
            let admission = if admitted {
                crate::code_mode_transcript::Admission::notification(
                    &codex_app_server_protocol::ServerNotification::ItemCompleted(
                        codex_app_server_protocol::ItemCompletedNotification::new(
                            owner.thread_id.to_string(),
                            String::new(),
                            codex_app_server_protocol::ThreadItemPayload::CodeModeOwned(owned),
                            None,
                            0,
                        ),
                    ),
                    crate::code_mode_transcript::Source::Live,
                )
                .unwrap()
                .unwrap()
            } else {
                crate::code_mode_transcript::Admission::item(owned, String::new())
            };
            reducers
                .admit(
                    owner.thread_id,
                    admission,
                    &cwd,
                    false,
                    &|_| crate::multi_agents::AgentMetadata::default(),
                    crate::history_cell::ImagePathOrigin::Local,
                )
                .unwrap()
                .block
        };
        let block = reduce(codex_app_server_protocol::ThreadItemPayload::CodeMode(
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
        let admission = || codex_app_server_protocol::ThreadItem {
            payload: codex_app_server_protocol::ThreadItemPayload::StepAdmission(
                codex_app_server_protocol::StepAdmissionItem {
                    id: "step:code".into(),
                    response_id: "code-response".into(),
                    read_tokens: 0,
                    thinking_tokens: None,
                    output_tokens: 0,
                    call_ids: vec!["code".into()],
                    message_ids: Vec::new(),
                    admission: codex_protocol::step_admission::Admission::Pending,
                },
            ),
            started_at_ms: None,
            completed_at_ms: None,
        };
        if admitted {
            view.record_step_admission(admission());
        }
        let mut code = vec![block];
        let buffer = tests::render(&mut view, &code, 104, 26);
        let shown = tests::text(&buffer);
        assert_title_color(&buffer, "Code Mode", admitted);
        let column_of = |line: &str, needle: &str| {
            unicode_width::UnicodeWidthStr::width(&line[..line.find(needle).unwrap()])
        };
        let header_column = column_of(
            shown
                .lines()
                .find(|line| line.contains("Code Mode"))
                .unwrap(),
            "Code Mode",
        );
        let pending = view.current_layout(&code, 0).unwrap();
        assert!(pending.text().starts_with("Code Mode"));
        assert!(!pending.text().contains('•'));
        assert!(!shown.contains("const answer"));
        for status_only in [false, true] {
            if status_only {
                code[0] = reduce(codex_app_server_protocol::ThreadItemPayload::ToolInput(
                    ToolInput {
                        id: "input:status-child".into(),
                        call_id: "status-child".into(),
                        name: ToolName::plain("update_work_status"),
                        payload: ToolInputPayload::Json(r#"{"activity":"Reading source"}"#.into()),
                    },
                ));
            }
            let mut empty_view = TranscriptView::default();
            if admitted {
                empty_view.record_step_admission(admission());
            }
            let buffer = tests::render(&mut empty_view, &code, 104, 26);
            let layout = empty_view.current_layout(&code, 0).unwrap();
            assert_eq!(layout.text(), pending.text());
            assert!(layout.fold_targets().iter().all(|target| !matches!(
                target,
                FoldTarget::Part(_)
                    | FoldTarget::Invocation {
                        action: Action::Format(Port::Input, OutputMode::Fmt),
                        ..
                    }
            )));
            let open_raw = FoldTarget::Invocation {
                call_id: "code".into(),
                action: Action::Format(Port::Input, OutputMode::Raw),
            };
            assert!(layout.fold_position(&open_raw).is_some());
            let shown = tests::text(&buffer);
            let header = shown
                .lines()
                .find(|line| line.contains("Code Mode"))
                .unwrap();
            assert!(header.contains("[in [raw]"));
            assert!(!header.contains("[fmt]"));
            for open in [false, true] {
                assert!(!header.contains(crate::render::disclosure::glyph(open).trim()));
            }
            assert!(empty_view.focus_fold(&code, 0, open_raw));
            let _ = empty_view.handle_key(KeyCode::Enter.into(), &code);
            drop(tests::render(&mut empty_view, &code, 104, 26));
            let opened = empty_view.current_layout(&code, 0).unwrap();
            assert!(opened.text().contains(script));
            assert!(
                opened
                    .fold_targets()
                    .iter()
                    .all(|target| !matches!(target, FoldTarget::Part(_)))
            );
        }
        code[0] = reduce(codex_app_server_protocol::ThreadItemPayload::ToolInput(
            ToolInput {
                id: "input:read-child".into(),
                call_id: "read-child".into(),
                name: ToolName::plain("fs"),
                payload: ToolInputPayload::Json(
                    r#"{"operation":"read","path":"src/main.rs","encoding":"utf8"}"#.into(),
                ),
            },
        ));
        tests::render(&mut view, &code, 104, 26);
        let first = view.current_layout(&code, 0).unwrap();
        assert!(first.text().contains("Read src/main.rs"));
        assert!(!first.text().contains("fs.readFile"));
        let format = FoldTarget::Invocation {
            call_id: "code".into(),
            action: Action::Format(Port::Input, OutputMode::Fmt),
        };
        assert!(pending.fold_position(&format).is_none());
        assert!(first.fold_position(&format).is_some());
        code[0] = reduce(codex_app_server_protocol::ThreadItemPayload::ToolInput(
            ToolInput {
                id: "input:status-child".into(),
                call_id: "status-child".into(),
                name: ToolName::plain("update_work_status"),
                payload: ToolInputPayload::Json(r#"{"activity":"Reading source"}"#.into()),
            },
        ));
        code[0] = reduce(codex_app_server_protocol::ThreadItemPayload::ToolInput(
            ToolInput {
                id: "input:write-child".into(),
                call_id: "write-child".into(),
                name: ToolName::plain("fs"),
                payload: ToolInputPayload::Json(
                    r#"{"operation":"write","path":"notes.txt","data":"line one\nline two"}"#
                        .into(),
                ),
            },
        ));
        // Canonical captures and semantic lifecycle cells may arrive in either order.
        // The capture must not replace the native formatter or duplicate its row.
        let mcp_input = codex_app_server_protocol::ThreadItemPayload::ToolInput(ToolInput {
            id: "input:mcp-child".into(),
            call_id: "mcp-child".into(),
            name: ToolName::plain("mcp__depot__sem_read_outline"),
            payload: ToolInputPayload::Json(r#"{"file":"src/main.rs"}"#.into()),
        });
        if admitted {
            code[0] = reduce(mcp_input.clone());
            tests::render(&mut view, &code, 104, 26);
            let awaiting_native = view.current_layout(&code, 0).unwrap();
            assert!(!awaiting_native.text().contains("mcp__depot"));
            assert!(!awaiting_native.text().contains(r#""file""#));
        }
        code[0] = reduce(codex_app_server_protocol::ThreadItemPayload::McpToolCall {
            id: "mcp-child".into(),
            server: "depot".into(),
            environment_id: None,
            tool: "sem.read.outline".into(),
            status: codex_app_server_protocol::McpToolCallStatus::InProgress,
            arguments: serde_json::json!({"file": "src/main.rs"}),
            app_context: None,
            mcp_app_resource_uri: None,
            mcp_app_ui: None,
            plugin_id: None,
            read_only_hint: Some(true),
            result: None,
            error: None,
            duration_ms: None,
        });
        if !admitted {
            code[0] = reduce(mcp_input);
        }
        code[0] = reduce(codex_app_server_protocol::ThreadItemPayload::ToolInput(
            ToolInput {
                id: "input:agent-child".into(),
                call_id: "agent-child".into(),
                name: ToolName::plain("spawn_agent"),
                payload: ToolInputPayload::Json(r#"{"message":"Inspect source"}"#.into()),
            },
        ));
        code[0] = reduce(
            codex_app_server_protocol::ThreadItemPayload::CollabAgentToolCall {
                id: "agent-child".into(),
                tool: codex_app_server_protocol::CollabAgentTool::SpawnAgent,
                status: codex_app_server_protocol::CollabAgentToolCallStatus::Completed,
                sender_thread_id: owner.thread_id.to_string(),
                receiver_thread_ids: vec![codex_protocol::ThreadId::from_u128(8).to_string()],
                prompt: Some("Inspect source".into()),
                model: None,
                reasoning_effort: None,
                agents_states: HashMap::default(),
                wait_completion: None,
            },
        );
        code[0] = reduce(codex_app_server_protocol::ThreadItemPayload::ToolInput(
            ToolInput {
                id: "input:command-child".into(),
                call_id: "command-child".into(),
                name: ToolName::plain("exec_command"),
                payload: ToolInputPayload::Json(r#"{"cmd":"cat Cargo.toml"}"#.into()),
            },
        ));
        code[0] = reduce(
            codex_app_server_protocol::ThreadItemPayload::CommandExecution {
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
            },
        );
        let buffer = tests::render(&mut view, &code, 104, 26);
        let shown = tests::text(&buffer);
        // Filesystem helpers and shell reads share semantic paint through the same Fmt surface.
        for label in ["Read src/main.rs", "Read Cargo.toml", "Write notes.txt"] {
            let (row, line) = shown
                .lines()
                .enumerate()
                .find(|(_, line)| line.contains(label))
                .unwrap();
            let column = line.find(label).unwrap();
            assert_eq!(
                buffer[(u16::try_from(column).unwrap(), u16::try_from(row).unwrap())].fg,
                ratatui::style::Color::Cyan
            );
            assert_eq!(
                buffer[(u16::try_from(column).unwrap(), u16::try_from(row).unwrap())].bg,
                ratatui::style::Color::Reset
            );
        }
        let nested = view.current_layout(&code, 0).unwrap();
        let block = code[0]
            .as_any()
            .downcast_ref::<crate::history_cell::CodeModeCell>()
            .unwrap();
        let native = block
            .members()
            .iter()
            .find(|cell| cell.as_any().is::<crate::history_cell::McpToolCallCell>())
            .unwrap();
        let native_call = crate::history_cell::invocation::collect(native.as_ref()).remove(0);
        let narrow = native_call.formatted_input(None, 1).unwrap();
        assert!(narrow.indicators.is_empty());
        assert_eq!(narrow.paint().lines[0].line.to_string(), "…");
        assert!(nested.text().contains("Read src/main.rs"));
        assert!(nested.text().contains("Read Cargo.toml"));
        assert!(!nested.text().contains("\"cmd\""));
        assert_eq!(nested.text().matches("Write notes.txt").count(), 1);
        assert_eq!(
            nested.text().matches("MCP depot.sem.read.outline").count(),
            1
        );
        assert!(
            nested
                .text()
                .contains(r#"MCP depot.sem.read.outline({"file":"src/main.rs"})"#)
        );
        assert!(nested.text().contains("Spawned"));
        assert!(!nested.text().contains("line one"));
        assert!(!nested.text().contains("update_work_status"));
        assert!(!nested.text().contains("Reading source"));
        assert!(!nested.text().contains("const answer"));
        assert!(!nested.text().contains("echo child"));
        assert!(nested.fold_position(&format).is_some());
        // A completed filesystem write publishes the same native edit as apply_patch.
        code[0] = reduce(codex_app_server_protocol::ThreadItemPayload::FileChange {
            id: "write-child".into(),
            changes: vec![codex_app_server_protocol::FileUpdateChange {
                path: "notes.txt".into(),
                kind: codex_app_server_protocol::PatchChangeKind::Add,
                diff: "+line one\n+line two\n".into(),
            }],
            status: codex_app_server_protocol::PatchApplyStatus::Completed,
        });
        let _ = tests::render(&mut view, &code, 104, 26);
        let edited = view.current_layout(&code, 0).unwrap();
        assert!(edited.text().contains("Edited"), "{}", edited.text());
        assert!(
            !edited.text().contains("Write notes.txt"),
            "{}",
            edited.text()
        );
        code[0] = reduce(
            codex_app_server_protocol::ThreadItemPayload::FunctionCallOutput {
                id: "code-output".into(),
                call_id: Some("code".into()),
                name: "exec".into(),
                namespace: None,
                output: FunctionCallOutputBody::Text("plain result, not code".into()),
            },
        );
        tests::render(&mut view, &code, 104, 26);
        let output = FoldTarget::Invocation {
            call_id: "code".into(),
            action: Action::Toggle(Port::Output, Panel::Visible(OutputMode::Raw)),
        };
        assert!(view.focus_fold(&code, 0, output));
        view.handle_key(KeyCode::Enter.into(), &code);
        let raw = FoldTarget::Invocation {
            call_id: "code".into(),
            action: Action::Format(Port::Input, OutputMode::Raw),
        };
        assert!(view.focus_fold(&code, 0, raw.clone()));
        view.handle_key(KeyCode::Enter.into(), &code);
        let buffer = tests::render(&mut view, &code, 104, 26);
        let shown = tests::text(&buffer);
        let (source_row, source_line) = shown
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("const answer"))
            .unwrap();
        let source_column = column_of(source_line, "const answer");
        assert_eq!(source_column, header_column + 2);
        assert_ne!(
            buffer[(
                u16::try_from(source_column).unwrap(),
                u16::try_from(source_row).unwrap()
            )]
                .fg,
            buffer[(
                u16::try_from(source_column + 15).unwrap(),
                u16::try_from(source_row).unwrap()
            )]
                .fg
        );
        assert!(
            view.current_layout(&code, 0)
                .unwrap()
                .text()
                .contains(script)
        );
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
            assert!(!collapsed.text().contains(script), "{:?}", collapsed.text());
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
                u16::try_from(screen_row).unwrap() + view.area.y
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
                        action: Action::Toggle(Port::Output, Panel::Hidden(OutputMode::Raw)),
                    })
                    .is_some()
            );
        }
        // Inventory navigation must reveal calls even inside a manually folded feed entry.
        assert!(view.focus_fold(&code, 0, outer.clone()));
        view.handle_key(KeyCode::Enter.into(), &code);
        tests::render(&mut view, &code, 104, 26);
        let collapsed = view.current_layout(&code, 0).unwrap();
        assert!(!collapsed.text().contains(script), "{:?}", collapsed.text());
        assert!(view.reveal_call(&code, "read-child"));
        tests::render(&mut view, &code, 104, 26);
        assert!(
            view.current_layout(&code, 0)
                .unwrap()
                .text()
                .contains(script)
        );
        assert!(view.reveal_call(&code, "code"));
        tests::render(&mut view, &code, 104, 26);
        assert!(
            view.current_layout(&code, 0)
                .unwrap()
                .text()
                .contains(script)
        );
        if !admitted {
            assert!(view.focus_fold(&code, 0, outer));
            view.handle_key(KeyCode::Enter.into(), &code);
            view.record_step_admission(admission());
            for width in [40, 80] {
                let buffer = tests::render(&mut view, &code, width, 26);
                assert_eq!(tests::text(&buffer).matches("Code Mode").count(), 1);
                let collapsed = view.current_layout(&code, 0).unwrap();
                assert!(!collapsed.text().contains(script), "{:?}", collapsed.text());
                assert_eq!(collapsed.row_count(), 1);
            }
            // Late billing rekeys the same disclosure, including its keyboard focus.
            view.handle_key(KeyCode::Enter.into(), &code);
            tests::render(&mut view, &code, 80, 26);
            let opened = view.current_layout(&code, 0).unwrap();
            assert!(opened.text().contains(script));
            assert!(opened.text().contains("plain result, not code"));
            assert!(opened.fold_position(&raw).is_some());
        }
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
    let mut view = TranscriptView::default();
    view.record_tool_input(input("python"));
    view.invocations.apply(
        "source",
        Action::Toggle(Port::Input, Panel::Visible(OutputMode::Raw)),
    );
    let python = view.invocations.body(
        &call,
        None,
        &view
            .invocations
            .prepared(&call, invocation::PreparationSource::Uncached),
        true,
        80,
        false,
    );
    assert_eq!(
        python
            .lines
            .iter()
            .map(|line| line.line.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        "print(42)"
    );
    let available = view.invocations.header(
        &call,
        None,
        &view
            .invocations
            .prepared(&call, invocation::PreparationSource::Uncached),
        80,
        true,
        80,
        None,
        false,
        crate::motion::MotionMode::Reduced,
    );
    assert!(available.controls.iter().all(|control| !matches!(
        control.target,
        FoldTarget::Invocation {
            action: Action::Format(_, OutputMode::Fmt),
            ..
        }
    )));
    assert!(
        python.lines[0]
            .line
            .spans
            .iter()
            .any(|span| span.style.fg != python.lines[0].line.spans[0].style.fg)
    );
    // Folded defaults must not hide explicit choices or export content.
    let input_text = |view: &TranscriptView, width, full| {
        view.invocations
            .body(
                &call,
                None,
                &view
                    .invocations
                    .prepared(&call, invocation::PreparationSource::Uncached),
                true,
                width,
                full,
            )
            .lines
            .into_iter()
            .map(|line| line.line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    for (name, payload, source) in [
        (
            "write_stdin",
            ToolInputPayload::Json(r#"{"session_id":7,"chars":""}"#.into()),
            "session_id",
        ),
        (
            "request_user_input",
            ToolInputPayload::Json(r#"{"questions":[{"question":"Continue?"}]}"#.into()),
            "Continue?",
        ),
        (
            "request_user_input_async",
            ToolInputPayload::Json(r#"{"questions":[{"title":"Continue?"}]}"#.into()),
            "Continue?",
        ),
        ("bash", ToolInputPayload::Text("echo 42".into()), "echo 42"),
        (
            "python",
            ToolInputPayload::Text("print(42)".into()),
            "print(42)",
        ),
    ] {
        let mut folded = TranscriptView::default();
        folded.record_tool_input(ToolInput {
            id: "input:source".into(),
            call_id: "source".into(),
            name: ToolName::plain(name),
            payload,
        });
        assert_eq!(input_text(&folded, 80, false), "");
        let full = input_text(&folded, 80, true);
        assert!(full.contains(source));
        assert!(full.contains("plain result, not Python"));
        folded.invocations.apply(
            "source",
            Action::Toggle(Port::Input, Panel::Visible(OutputMode::Fmt)),
        );
        for width in [40, 80] {
            assert!(input_text(&folded, width, false).contains(source));
        }
    }
    let mut opaque = TranscriptView::default();
    opaque.record_tool_input(input("opaque"));
    opaque.invocations.apply(
        "source",
        Action::Toggle(Port::Input, Panel::Visible(OutputMode::Raw)),
    );
    let reserved = opaque.invocations.header(
        &call,
        None,
        &opaque
            .invocations
            .prepared(&call, invocation::PreparationSource::Uncached),
        80,
        true,
        80,
        None,
        false,
        crate::motion::MotionMode::Reduced,
    );
    assert!(
        reserved
            .lines
            .iter()
            .any(|line| line.line.to_string().contains("[raw]      ]"))
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
    // Animation keys invalidate paint, not source preparation; hidden panes stay unbuilt.
    let mut reuse = TranscriptView::default();
    reuse.record_tool_input(ToolInput {
        id: "input:source".into(),
        call_id: "source".into(),
        name: ToolName::plain("bash"),
        payload: ToolInputPayload::Text("echo sample".into()),
    });
    let key = |revision, tick| {
        Some(ActiveCellTranscriptKey {
            cacheable: true,
            revision,
            is_stream_continuation: false,
            is_tool_entry: true,
            animation_tick: Some(tick),
        })
    };
    for tick in 0..4 {
        assert_eq!(
            reuse.sync_live_invocation_tail(80, key(11, tick), &output, vec![]),
            Some(true)
        );
    }
    let prepared = reuse
        .invocations
        .prepared(&call, invocation::PreparationSource::Live(11));
    assert_eq!(prepared.work(), [1, 1, 0, 0]);
    reuse
        .invocations
        .apply("source", Action::Format(Port::Input, OutputMode::Raw));
    reuse
        .invocations
        .apply("source", Action::Format(Port::Output, OutputMode::Raw));
    for tick in 4..8 {
        reuse.sync_live_invocation_tail(80, key(11, tick), &output, vec![]);
    }
    assert_eq!(prepared.work(), [1, 1, 1, 1]);
    let live = reuse.live.as_ref().unwrap().text();
    assert!(live.contains("echo sample"));
    assert!(live.contains("plain result, not Python"));
    reuse.sync_live_invocation_tail(40, key(11, 8), &output, vec![]);
    assert_eq!(prepared.work(), [2, 1, 1, 2]);
    reuse.sync_live_invocation_tail(80, key(11, 9), &output, vec![]);
    assert_eq!(prepared.work(), [2, 1, 1, 2]);
    let updated = ToolOutputCell::from_output(
        "source-updated",
        Some("source"),
        "bash",
        None,
        &FunctionCallOutputBody::Text("updated result".into()),
    )
    .unwrap();
    reuse.sync_live_invocation_tail(80, key(12, 10), &updated, vec![]);
    assert!(
        reuse
            .live
            .as_ref()
            .unwrap()
            .text()
            .contains("updated result")
    );
    let updated_call = crate::history_cell::invocation::collect(&updated).remove(0);
    let changed = reuse
        .invocations
        .prepared(&updated_call, invocation::PreparationSource::Live(12));
    assert!(!std::rc::Rc::ptr_eq(&prepared, &changed));
    assert_eq!(changed.work(), [1, 1, 1, 1]);
    reuse.record_tool_input(ToolInput {
        id: "input:source".into(),
        call_id: "source".into(),
        name: ToolName::plain("bash"),
        payload: ToolInputPayload::Text("echo changed".into()),
    });
    reuse.sync_live_invocation_tail(80, key(12, 11), &updated, vec![]);
    assert!(reuse.live.as_ref().unwrap().text().contains("echo changed"));
    assert!(!std::rc::Rc::ptr_eq(
        &changed,
        &reuse
            .invocations
            .prepared(&updated_call, invocation::PreparationSource::Live(12),)
    ));
    // A final delimiter remains copyable without becoming an extra painted row.
    for source in ["print(42)\n", "print(42)\n\n"] {
        let input = ToolInput {
            id: "input:source".into(),
            call_id: "source".into(),
            name: ToolName::plain("python"),
            payload: ToolInputPayload::Text(source.into()),
        };
        let lines = call.raw_input(Some(&input)).unwrap();
        let layout = TextLayout::new(lines, 80);
        assert_eq!(layout.text(), source);
        assert_eq!(layout.row_count(), source.lines().count());
        let narrow = layout.rewrap(6);
        assert_eq!(narrow.text(), source);
        assert_eq!(narrow.rewrap(80).row_count(), layout.row_count());
    }
}

#[test]
fn grouped_invocation_ports_stay_within_their_feed_item() {
    use crossterm::event::KeyModifiers;
    use crossterm::event::MouseButton;
    use crossterm::event::MouseEvent;
    use crossterm::event::MouseEventKind;

    fn call(view: &mut TranscriptView, id: &str) -> Arc<dyn HistoryCell> {
        view.record_tool_input(ToolInput {
            id: format!("input:{id}"),
            call_id: id.into(),
            name: ToolName::plain("custom"),
            payload: ToolInputPayload::Json(format!(r#"{{"input":"{id}","n":12}}"#)),
        });
        Arc::new(
            ToolOutputCell::from_output(
                &format!("output:{id}"),
                Some(id),
                "custom",
                None,
                &FunctionCallOutputBody::Text(format!(r#"{{"output":"{id}","n":34}}"#)),
            )
            .unwrap(),
        )
    }

    fn panel(
        view: &mut TranscriptView,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        port: Port,
    ) -> Panel {
        view.current_layout(cells, index)
            .unwrap()
            .fold_targets()
            .into_iter()
            .find_map(|target| match target {
                FoldTarget::Invocation {
                    action: Action::Toggle(found, next),
                    ..
                } if found == port => Some(next.toggled()),
                _ => None,
            })
            .unwrap()
    }

    fn click(
        view: &mut TranscriptView,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        port: Port,
        mode: Option<OutputMode>,
        modifiers: KeyModifiers,
    ) {
        let width = view.area.width;
        tests::render(view, cells, width, 100);
        let layout = view.current_layout(cells, index).unwrap();
        let target = layout
            .fold_targets()
            .into_iter()
            .find(|target| match (target, mode) {
                (
                    FoldTarget::Invocation {
                        action: Action::Toggle(found, _),
                        ..
                    },
                    None,
                ) => *found == port,
                (
                    FoldTarget::Invocation {
                        action: Action::Format(found, choice),
                        ..
                    },
                    Some(mode),
                ) => *found == port && *choice == mode,
                _ => false,
            })
            .unwrap();
        let (row, columns, _) = layout.fold_position(&target).unwrap();
        let screen = view
            .visible
            .iter()
            .position(|visible| visible.index == index && visible.row == row)
            .unwrap();
        assert!(matches!(
            view.handle_mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: columns.start,
                    row: u16::try_from(screen).unwrap(),
                    modifiers,
                },
                cells
            ),
            Some(ViewAction::Changed)
        ));
        let width = view.area.width;
        tests::render(view, cells, width, 100);
    }

    let mut view = TranscriptView::default();
    let mut cells: Vec<_> = ["a", "b", "c", "d", "e", "f"]
        .map(|id| call(&mut view, id))
        .into();
    for (response, ids) in [
        ("group", vec!["a", "b", "older", "new"]),
        ("other", vec!["c", "d"]),
        ("single-e", vec!["e"]),
        ("single-f", vec!["f"]),
    ] {
        view.record_step_admission(codex_app_server_protocol::ThreadItem {
            payload: codex_app_server_protocol::ThreadItemPayload::StepAdmission(
                codex_app_server_protocol::StepAdmissionItem {
                    id: format!("step:{response}"),
                    response_id: response.into(),
                    read_tokens: 0,
                    thinking_tokens: None,
                    output_tokens: 0,
                    call_ids: ids.into_iter().map(str::to_owned).collect(),
                    message_ids: Vec::new(),
                    admission: codex_protocol::step_admission::Admission::Pending,
                },
            ),
            started_at_ms: None,
            completed_at_ms: None,
        });
    }
    tests::render(&mut view, &cells, 104, 100);
    let untouched: Vec<_> = (2..6)
        .map(|index| {
            (
                panel(&mut view, &cells, index, Port::Input),
                panel(&mut view, &cells, index, Port::Output),
            )
        })
        .collect();
    let ctrl = KeyModifiers::CONTROL;
    let plain = KeyModifiers::NONE;

    click(&mut view, &cells, 0, Port::Input, None, ctrl);
    for index in 0..2 {
        assert!(panel(&mut view, &cells, index, Port::Input).visible());
        assert!(!panel(&mut view, &cells, index, Port::Output).visible());
    }
    click(&mut view, &cells, 1, Port::Input, None, plain);
    assert!(panel(&mut view, &cells, 0, Port::Input).visible());
    assert!(!panel(&mut view, &cells, 1, Port::Input).visible());
    click(&mut view, &cells, 0, Port::Input, None, ctrl);
    click(&mut view, &cells, 0, Port::Input, None, ctrl);
    assert!(panel(&mut view, &cells, 1, Port::Input).visible());

    click(
        &mut view,
        &cells,
        0,
        Port::Input,
        Some(OutputMode::Raw),
        ctrl,
    );
    click(
        &mut view,
        &cells,
        1,
        Port::Input,
        Some(OutputMode::Fmt),
        plain,
    );
    click(&mut view, &cells, 0, Port::Output, None, ctrl);
    click(
        &mut view,
        &cells,
        0,
        Port::Output,
        Some(OutputMode::Fmt),
        ctrl,
    );
    for index in 0..2 {
        assert_eq!(
            panel(&mut view, &cells, index, Port::Output),
            Panel::Visible(OutputMode::Fmt)
        );
    }
    click(
        &mut view,
        &cells,
        0,
        Port::Output,
        Some(OutputMode::Raw),
        ctrl,
    );
    // Visibility is a separate choice: group open/close preserves each member's format.
    click(&mut view, &cells, 0, Port::Input, None, ctrl);
    click(&mut view, &cells, 1, Port::Input, None, ctrl);
    assert_eq!(
        panel(&mut view, &cells, 0, Port::Input),
        Panel::Visible(OutputMode::Raw)
    );
    assert_eq!(
        panel(&mut view, &cells, 1, Port::Input),
        Panel::Visible(OutputMode::Fmt)
    );
    for index in 0..2 {
        assert_eq!(
            panel(&mut view, &cells, index, Port::Output),
            Panel::Visible(OutputMode::Raw)
        );
    }

    // Workers keep immutable choices, while later arrivals inherit the current item default.
    let worker = view
        .invocations
        .snapshot(cells.iter().map(AsRef::as_ref))
        .restore();
    let scope = view.feed.fold_of(0).unwrap().to_owned();
    click(&mut view, &cells, 0, Port::Output, None, ctrl);
    let invocation = crate::history_cell::invocation::collect(cells[0].as_ref()).remove(0);
    let old_body = worker.body(
        &invocation,
        Some(&scope),
        &worker.prepared(&invocation, invocation::PreparationSource::Uncached),
        true,
        80,
        false,
    );
    assert!(
        old_body
            .lines
            .iter()
            .any(|line| line.line.to_string().contains(r#""output":"a""#))
    );
    cells.insert(2, call(&mut view, "new"));
    tests::render(&mut view, &cells, 104, 100);
    assert_eq!(
        panel(&mut view, &cells, 2, Port::Input),
        Panel::Visible(OutputMode::Raw)
    );
    assert!(!panel(&mut view, &cells, 2, Port::Output).visible());
    for (index, expected) in (3..7).zip(untouched) {
        assert_eq!(
            (
                panel(&mut view, &cells, index, Port::Input),
                panel(&mut view, &cells, index, Port::Output),
            ),
            expected
        );
    }

    // A Feed Item's own port strip is never a bulk action, even when it wraps.
    click(&mut view, &cells, 5, Port::Output, None, ctrl);
    click(
        &mut view,
        &cells,
        5,
        Port::Output,
        Some(OutputMode::Raw),
        ctrl,
    );
    assert!(panel(&mut view, &cells, 5, Port::Output).visible());
    for index in [0, 1, 2, 3, 4, 6] {
        assert!(!panel(&mut view, &cells, index, Port::Output).visible());
    }
    cells.insert(0, call(&mut view, "older"));
    for width in [48, 104] {
        tests::render(&mut view, &cells, width, 100);
        assert_eq!(
            panel(&mut view, &cells, 0, Port::Input),
            Panel::Visible(OutputMode::Raw)
        );
        assert!(!panel(&mut view, &cells, 0, Port::Output).visible());
        assert_eq!(
            panel(&mut view, &cells, 2, Port::Input),
            Panel::Visible(OutputMode::Fmt)
        );
        click(&mut view, &cells, 6, Port::Output, None, ctrl);
        assert!(!panel(&mut view, &cells, 7, Port::Output).visible());
    }
}
