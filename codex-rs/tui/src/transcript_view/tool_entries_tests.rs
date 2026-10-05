//! Empty polling results and opaque subagent events are still compact tool entries.

use super::*;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::ToolOutputCell;
use codex_app_server_protocol::SubAgentActivityKind;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use pretty_assertions::assert_eq;

#[test]
fn completed_thought_discloses_only_readable_reasoning() {
    let cwd = std::path::Path::new("/tmp");
    let mut snapshots = Vec::new();
    for (label, tokens, summary, hidden) in [
        (
            "counted",
            Some(1300),
            vec!["First block", "Second block"],
            false,
        ),
        ("unknown", None, vec!["Readable summary"], false),
        ("encrypted-only", Some(1300), Vec::new(), false),
        ("opaque-unknown", None, Vec::new(), false),
        ("hidden", Some(1300), vec!["Private summary"], true),
    ] {
        let cell = Arc::new(crate::history_cell::ThoughtCell::from_item(
            ThreadItem::Thought {
                id: format!("thought:{label}"),
                response_id: label.into(),
                completed_at_ms: 1_700_000_000_000,
                summary: summary.into_iter().map(str::to_owned).collect(),
                content: Vec::new(),
                reported_reasoning_tokens: tokens,
            },
            cwd,
            if hidden {
                crate::history_cell::ThoughtTextVisibility::Hidden
            } else {
                crate::history_cell::ThoughtTextVisibility::Summary
            },
        ));
        let cells: Vec<Arc<dyn HistoryCell>> = vec![cell.clone()];
        let mut view = TranscriptView::default();
        let clock = crate::history_cell::HistoryTimestamp::from_millis(1_700_000_000_000)
            .unwrap()
            .span()
            .content
            .trim()
            .to_owned();
        for expanded in [false, true] {
            if expanded && cell.activity_disclosure(80).is_none() {
                continue;
            }
            if expanded {
                view.disclosure.expanded.extend(cell.activity_ids());
            }
            let buffer = tests::render(&mut view, &cells, /*width*/ 64, /*height*/ 6);
            snapshots.push(format!(
                "{label} expanded={expanded}\n{}",
                tests::text(&buffer).replace(&clock, "HH:MM:SS")
            ));
            let length = view
                .current_layout(&cells, /*index*/ 0)
                .unwrap()
                .text()
                .len();
            view.begin_selection(&cells, /*column*/ 6, /*row*/ 0, /*clicks*/ 1);
            let selection = view.selection.as_mut().unwrap();
            selection.start.offset = 0;
            selection.end.offset = length;
            let copied = view.selected_text(&cells).unwrap();
            assert!(copied.contains("Thought") && !copied.contains("tokens"));
            view.end_selection(&cells);
        }
        if cell.activity_disclosure(20).is_none() {
            let buffer = tests::render(&mut view, &cells, /*width*/ 20, /*height*/ 1);
            snapshots.push(format!(
                "{label} narrow\n{}",
                tests::text(&buffer).replace(&clock, "HH:MM:SS")
            ));
        }
    }
    insta::assert_snapshot!(snapshots.join("\n"));
}

#[test]
fn captured_output_modes_preserve_blocks_lexemes_and_wrapped_copy() {
    use crate::history_cell::activity_parts::FoldTarget;
    use crate::history_cell::output_provenance::OutputMode;
    use crate::history_cell::output_provenance::OutputProvenance;
    use crate::history_cell::output_provenance::client_notice;
    let json = r#"{"a":9007199254740993123456789,"a":1.2300e+04,"text":"first\nsecond\tend","literal":"\\n","key\n":"\u0061"}"#;
    let plain = "prefix {\"embedded\":true}";
    let invalid = "{\"truncated\":";
    let body = FunctionCallOutputBody::ContentItems(
        [json, plain, invalid]
            .map(|text| FunctionCallOutputContentItem::InputText { text: text.into() })
            .into(),
    );
    let output = Arc::new(
        ToolOutputCell::from_output("mode", Some("mode"), "tool", /*namespace*/ None, &body)
            .unwrap(),
    );
    let cells: Vec<Arc<dyn HistoryCell>> = vec![output.clone()];
    let mut view = TranscriptView::default();
    view.disclosure.expanded.extend(output.activity_ids());
    view.record_tool_output_tokens("tool-output-tokens:mode".into(), "mode".into(), Some(123));
    let mut painted = Vec::new();
    for mode in [OutputMode::Fmt, OutputMode::Raw, OutputMode::Fmt] {
        tests::render(&mut view, &cells, /*width*/ 80, /*height*/ 14);
        let layout = view.current_layout(&cells, /*index*/ 0).unwrap();
        let target = FoldTarget::OutputMode(mode);
        let (row, columns, _) = layout.fold_position(&target).unwrap();
        assert_eq!(layout.fold_at(row, columns.start), Some(target));
        assert_eq!(layout.fold_at(/*row*/ 1, /*column*/ 10), None);
        assert!(view.toggle_disclosure_at(&cells, columns.start, row as u16));
        let buffer = tests::render(&mut view, &cells, /*width*/ 80, /*height*/ 14);
        let layout = view.current_layout(&cells, /*index*/ 0).unwrap();
        let content_start = layout.text().find('\n').unwrap() + 1;
        let text = layout.text()[content_start..].to_owned();
        assert!(text.ends_with(&format!("\n{plain}\n{invalid}")));
        if mode == OutputMode::Raw {
            assert_eq!(text, [json, plain, invalid].join("\n"));
        } else {
            assert!(text.contains("9007199254740993123456789"));
            assert!(text.contains("1.2300e+04"));
            assert!(text.contains("\"text\": \"first\nsecond\tend\""));
            assert!(text.contains(r#""literal": "\\n""#));
            assert!(text.contains(r#""key\n": "\u0061""#));
            assert_eq!(text.matches("\"a\":").count(), 2);
        }
        view.begin_selection(&cells, /*column*/ 4, /*row*/ 1, /*clicks*/ 1);
        let selection = view.selection.as_mut().unwrap();
        selection.start.offset = content_start;
        selection.end.offset = layout.text().len();
        tests::render(&mut view, &cells, /*width*/ 34, /*height*/ 18);
        assert_eq!(view.selected_text(&cells), Some(text));
        view.end_selection(&cells);
        painted.push(format!("{mode:?}\n{buffer:?}"));
    }
    assert!(view.focus_fold(
        &cells,
        /*index*/ 0,
        FoldTarget::OutputMode(OutputMode::Raw)
    ));
    for (key, mode) in [
        (crossterm::event::KeyCode::Left, OutputMode::Raw),
        (crossterm::event::KeyCode::Right, OutputMode::Fmt),
    ] {
        view.handle_key(
            crossterm::event::KeyEvent::new(key, crossterm::event::KeyModifiers::NONE),
            &cells,
        );
        assert_eq!(view.disclosure.output_mode(&output.activity_ids()), mode);
        assert!(view.disclosure.is_expanded(&output.activity_ids()));
    }
    view.handle_key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ),
        &cells,
    );
    let older: Arc<dyn HistoryCell> = Arc::new(
        ToolOutputCell::from_output(
            "older-mode",
            Some("mode"),
            "tool",
            /*namespace*/ None,
            &FunctionCallOutputBody::Text("older output".into()),
        )
        .unwrap(),
    );
    let regrouped = vec![crate::history_cell::ToolOutputGroup::from_cells([
        older,
        output.clone() as Arc<dyn HistoryCell>,
    ])];
    tests::render(&mut view, &regrouped, /*width*/ 80, /*height*/ 20);
    assert_eq!(
        view.disclosure.output_mode(&regrouped[0].activity_ids()),
        OutputMode::Raw
    );
    view.handle_key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Right,
            crossterm::event::KeyModifiers::NONE,
        ),
        &regrouped,
    );
    assert_eq!(
        view.disclosure.output_mode(&output.activity_ids()),
        OutputMode::Fmt
    );
    view.sync_live_activity(&[], output.activity_ids());
    let mut live_modes = Vec::new();
    for key in [
        crossterm::event::KeyCode::Left,
        crossterm::event::KeyCode::Right,
    ] {
        view.handle_key(
            crossterm::event::KeyEvent::new(key, crossterm::event::KeyModifiers::NONE),
            &cells,
        );
        view.sync_live_activity_tail(
            /*width*/ 80,
            /*key*/ None,
            /*expanded*/ true,
            |width, _, mode| {
                live_modes.push(mode);
                Some(ActivityTranscriptLines {
                    output_mode: Some(mode),
                    is_tool_entry: true,
                    activity: output.expanded_output_lines(width, mode),
                    header: output.activity_header(),
                    auxiliary: Vec::new(),
                    header_tokens: None,
                    disclosure: output.activity_disclosure(width),
                    controls: Vec::new(),
                })
            },
        );
    }
    assert_eq!(live_modes, vec![OutputMode::Raw, OutputMode::Fmt]);
    let mut surfaces = output.output_lines(/*width*/ 80, OutputMode::Fmt);
    surfaces.push(client_notice("Stored result · exact capture unavailable"));
    let layout = TextLayout::new(surfaces, /*width*/ 80);
    assert!(!layout.text().contains("Stored result"));
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 14,
    );
    let mut buffer = Buffer::empty(area);
    layout.render(area, &mut buffer, /*start_row*/ 0);
    painted.push(format!("provenance surfaces\n{buffer:?}"));
    let narrow = layout.rewrap(/*width*/ 24);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 24, /*height*/ 24,
    );
    let mut buffer = Buffer::empty(area);
    narrow.render(area, &mut buffer, /*start_row*/ 0);
    assert!(tests::text(&buffer).contains("Stored result"));
    assert!(!narrow.text().contains("Stored result"));
    assert!(
        output
            .output_lines(/*width*/ 80, OutputMode::Fmt)
            .iter()
            .any(|line| line.source.as_ref().unwrap().output_provenance
                == OutputProvenance::Formatted)
    );
    // Safety must remain a visible, copyable projection even when JSON caps reject the capture.
    let unsafe_text = "\u{1b}[31munsafe\u{7}\rend\u{7f}\u{85}\nlast\tcolumn";
    let safe_text = r"\u{1b}[31munsafe\u{7}\rend\u{7f}\u{85}".to_owned() + "\nlast\tcolumn";
    for (source, safe) in [
        (unsafe_text.to_owned(), safe_text.clone()),
        (
            "{\r\"key\":true}".into(),
            r"{\r".to_owned() + "\"key\":true}",
        ),
        (
            format!("{unsafe_text}{}", "x".repeat(256 * 1024)),
            format!("{safe_text}{}", "x".repeat(256 * 1024)),
        ),
    ] {
        let paint = source == unsafe_text;
        let output = Arc::new(
            ToolOutputCell::from_output(
                "unsafe",
                Some("unsafe"),
                "tool",
                /*namespace*/ None,
                &FunctionCallOutputBody::Text(source),
            )
            .unwrap(),
        );
        assert!(!output.has_formatted_output());
        for mode in [OutputMode::Raw, OutputMode::Fmt] {
            let lines = output.output_lines(u16::MAX, mode);
            assert_eq!(
                lines
                    .first()
                    .unwrap()
                    .source
                    .as_ref()
                    .unwrap()
                    .output_provenance,
                OutputProvenance::Formatted
            );
            assert_eq!(
                lines
                    .last()
                    .unwrap()
                    .source
                    .as_ref()
                    .unwrap()
                    .output_provenance,
                OutputProvenance::Client
            );
            let layout = TextLayout::new(lines, u16::MAX);
            assert_eq!(layout.text(), format!("{safe}\n"));
        }
        if !paint {
            continue;
        }
        let cells: Vec<Arc<dyn HistoryCell>> = vec![output.clone()];
        let mut view = TranscriptView::default();
        view.disclosure.expanded.extend(output.activity_ids());
        tests::render(&mut view, &cells, /*width*/ 32, /*height*/ 12);
        let layout = view.current_layout(&cells, /*index*/ 0).unwrap();
        let start = layout.text().find(&safe).unwrap();
        view.begin_selection(&cells, /*column*/ 4, /*row*/ 1, /*clicks*/ 1);
        let selection = view.selection.as_mut().unwrap();
        selection.start.offset = start;
        selection.end.offset = start + safe.len();
        tests::render(&mut view, &cells, /*width*/ 24, /*height*/ 16);
        assert_eq!(view.selected_text(&cells), Some(safe));
        view.end_selection(&cells);
        let buffer = tests::render(&mut view, &cells, /*width*/ 32, /*height*/ 12);
        assert!(tests::text(&buffer).contains("escaped for display"));
        painted.push(format!("escaped capture\n{buffer:?}"));
    }
    insta::assert_snapshot!(painted.join("\n"));
}

#[test]
fn mixed_feed_keeps_its_grid_through_live_commit_resize_and_details() {
    use crate::diff_model::FileChange;
    use crate::exec_cell::CommandOutput;
    use crate::exec_cell::new_active_exec_command;
    use crate::history_cell::AgentMarkdownCell;
    use crate::history_cell::HistoryTimestamp;
    use crate::history_cell::McpInvocation;
    use crate::history_cell::PrefixedWrappedHistoryCell;
    use crate::history_cell::ToolOutputGroup;
    use crate::history_cell::new_active_mcp_tool_call;
    use crate::history_cell::new_patch_events;
    use crate::history_cell::new_user_prompt;
    use codex_app_server_protocol::CommandExecutionSource;
    use codex_shell_command::parse_command::parse_command;
    use std::collections::HashMap;
    use std::path::Path;
    use std::time::Duration;

    let cwd = Path::new(".");
    let timestamp = HistoryTimestamp::from_millis(1_700_000_000_000);
    let prose = |source: &str| {
        let mut cell = AgentMarkdownCell::new(source.into(), cwd);
        cell.timestamp = timestamp;
        Arc::new(cell) as Arc<dyn HistoryCell>
    };
    let command = vec!["bash".into(), "-lc".into(), "cat foo.rs".into()];
    let mut shell = new_active_exec_command(
        "shell".into(),
        command.clone(),
        parse_command(&command),
        CommandExecutionSource::UnifiedExecStartup,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    shell.complete_call(
        "shell",
        CommandOutput::new(/*exit_code*/ 0, "source".into()),
        Duration::ZERO,
    );
    shell.completed_at = timestamp;
    let mut poll = new_active_exec_command(
        "poll".into(),
        vec!["bash".into(), "-lc".into(), "poll foo".into()],
        Vec::new(),
        CommandExecutionSource::UnifiedExecInteraction,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    poll.complete_call(
        "poll",
        CommandOutput::new(/*exit_code*/ 0, String::new()),
        Duration::ZERO,
    );
    let mut mcp = new_active_mcp_tool_call(
        "mcp".into(),
        McpInvocation {
            server: "tools".into(),
            tool: "inspect".into(),
            arguments: None,
        },
        /*animations_enabled*/ false,
    );
    mcp.complete(
        Duration::ZERO,
        Ok(codex_protocol::mcp::CallToolResult {
            content: Vec::new(),
            structured_content: None,
            is_error: None,
            meta: None,
        }),
    );
    mcp.completed_at = timestamp;
    let batch = ToolOutputCell::from_output(
        "batch",
        Some("outer"),
        "exec",
        /*namespace*/ None,
        &FunctionCallOutputBody::Text("combined result".into()),
    )
    .unwrap();
    let group = ToolOutputGroup::from_cells([
        Arc::new(poll) as Arc<dyn HistoryCell>,
        Arc::new(shell) as Arc<dyn HistoryCell>,
        Arc::new(mcp),
        Arc::new(batch),
    ]);
    let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
        Arc::new(new_user_prompt(
            "Review the layout".into(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )),
        Arc::new(PrefixedWrappedHistoryCell::new(
            "Activity Reviewing layout\n\nMore text",
            crate::history_cell::agent_prefix(timestamp, /*first*/ true),
            crate::history_cell::agent_prefix(timestamp, /*first*/ false),
        )),
        group,
    ];
    // Distinct calls coalesce only their Edited heading, never their file controls or bills.
    for file in ["foo.rs", "bar.rs"] {
        cells.extend(
            new_patch_events(
                HashMap::from([(
                    file.into(),
                    FileChange::Add {
                        content: "new line\n".into(),
                    },
                )]),
                cwd,
            )
            .into_iter()
            .map(|cell| Arc::new(cell) as Arc<dyn HistoryCell>),
        );
    }
    let tail = prose(
        "The layout keeps prose, code, and tools aligned.\n\n```rust\n  source_indent();\n``` ",
    );
    let mut view = TranscriptView::default();
    view.record_tool_output_tokens("tool-output-tokens:batch".into(), "outer".into(), Some(128));
    let mut snapshots = Vec::new();
    for width in [64, 32] {
        view.set_presentation(/*detailed*/ false, HistoryRenderMode::Rich);
        view.jump_to_latest();
        // Normal live prose travels through the auxiliary activity-presentation boundary.
        view.sync_live_activity_tail(
            width,
            /*key*/ None,
            /*expanded*/ false,
            |width, _, _| {
                Some(ActivityTranscriptLines {
                    output_mode: None,
                    is_tool_entry: false,
                    header: None,
                    activity: Vec::new(),
                    auxiliary: tail.compact_hyperlink_lines(width),
                    header_tokens: None,
                    disclosure: None,
                    controls: Vec::new(),
                })
            },
        );
        let live = tests::render(&mut view, &cells, width, /*height*/ 50);
        view.sync_live_tail(width, /*key*/ None, |_| None);
        cells.push(Arc::clone(&tail));
        let committed = tests::render(&mut view, &cells, width, /*height*/ 50);
        assert_eq!(live, committed);
        let text = tests::text(&committed);
        assert_eq!(text.matches("128 tokens").count(), usize::from(width == 64));
        assert_eq!(text.matches("Edited").count(), 1);
        let layout = view.current_layout(&cells, cells.len() - 1).unwrap();
        assert!(layout.text().contains("  source_indent();"));
        assert!(!layout.text().starts_with(' '));
        snapshots.push(format!("width={width}\n{}", text.trim_end()));
        if width == 64 {
            view.disclosure.expanded.extend(cells[2].activity_ids());
            let expanded = tests::render(&mut view, &cells, width, /*height*/ 50);
            let text = tests::text(&expanded);
            assert!(text.contains("$ poll foo"));
            assert!(!text.contains("tokens"));
            snapshots.push(format!("expanded width={width}\n{}", text.trim_end()));
            view.disclosure.expanded.clear();
        }
        view.set_presentation(/*detailed*/ true, HistoryRenderMode::Rich);
        view.jump_to_latest();
        let detailed = tests::render(&mut view, &cells, width, /*height*/ 50);
        snapshots.push(format!(
            "detailed width={width}\n{}",
            tests::text(&detailed).trim_end()
        ));
        cells.pop();
    }
    let clock = timestamp.unwrap().span().content.trim().to_owned();
    insta::assert_snapshot!(snapshots.join("\n\n").replace(&clock, "14:17:54"));
    view.set_presentation(/*detailed*/ false, HistoryRenderMode::Raw);
    view.jump_to_latest();
    let raw = tests::render(&mut view, &cells, /*width*/ 64, /*height*/ 50);
    assert!(tests::text(&raw).starts_with("Review the layout\n"));
}

#[test]
fn waits_and_agent_events_preserve_captured_text_without_extra_gaps() {
    let process = |state: Option<i64>| {
        serde_json::json!({
            "chunk_id": "sample", "wall_time_seconds": 5.0, "output": "",
            "exit_code": state, "session_id": if state.is_none() { Some(42) } else { None },
        })
        .to_string()
    };
    let running = "Script running with cell ID 7\nWall time 1.0 seconds\nOutput:\n";
    let completed = "Script completed\nWall time 5.0 seconds\nOutput:\n";
    let cases = [
        (
            "exec",
            FunctionCallOutputBody::Text(format!("{completed}{}", process(None))),
            47,
        ),
        ("exec", FunctionCallOutputBody::Text(running.into()), 18),
        ("wait", FunctionCallOutputBody::Text(running.into()), 18),
        // Code Mode can split the envelope and process result into separate text blocks.
        (
            "wait",
            FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText {
                    text: completed.into(),
                },
                FunctionCallOutputContentItem::InputText {
                    text: process(Some(0)),
                },
            ]),
            47,
        ),
        (
            "exec",
            FunctionCallOutputBody::Text(format!("{completed}{}", process(Some(2)))),
            47,
        ),
        ("wait", FunctionCallOutputBody::Text(completed.into()), 13),
        (
            "wait",
            FunctionCallOutputBody::Text(
                "Script terminated\nWall time 1.0 seconds\nOutput:\n".into(),
            ),
            13,
        ),
        (
            "exec",
            FunctionCallOutputBody::Text("a computed value".into()),
            4,
        ),
        (
            "wait",
            FunctionCallOutputBody::ContentItems(vec![FunctionCallOutputContentItem::InputText {
                text: "Script completed\nOutput:\n".into(),
            }]),
            7,
        ),
        (
            "exec",
            FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "Script completed\nOutput:\n".into(),
                },
                FunctionCallOutputContentItem::InputText {
                    text: "Script failed\nOutput:\n".into(),
                },
            ]),
            14,
        ),
        (
            "exec",
            FunctionCallOutputBody::Text("Script completed\nOutput:\n".into()),
            7,
        ),
    ];
    let agent = |kind| {
        Arc::new(
            crate::multi_agents::sub_agent_activity_history_cell(&ThreadItem::SubAgentActivity {
                id: format!("{kind:?}"),
                kind,
                agent_thread_id: "00000000-0000-0000-0000-000000000001".into(),
                agent_path: "/root/worker".into(),
                message_preview: None,
            })
            .unwrap(),
        ) as Arc<dyn HistoryCell>
    };
    let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
        Arc::new(PlainHistoryCell::new(vec!["Before tools".into()])),
        agent(SubAgentActivityKind::Started),
    ];
    let mut view = TranscriptView::default();
    for (index, (name, output, tokens)) in cases.iter().enumerate() {
        let id = format!("poll-{index}");
        let cell =
            ToolOutputCell::from_output(&id, Some(&id), name, /*namespace*/ None, output).unwrap();
        let header_only =
            matches!(output, FunctionCallOutputBody::ContentItems(items) if items.len() == 1);
        assert_eq!(
            cell.activity_disclosure(/*width*/ 100).is_some(),
            !header_only
        );
        view.record_tool_output_tokens(format!("tool-output-tokens:{id}"), id, Some(*tokens));
        cells.push(Arc::new(cell));
        if index == 1 {
            cells.push(agent(SubAgentActivityKind::Interacted));
        }
        if index == 4 {
            cells.push(agent(SubAgentActivityKind::Interrupted));
        }
    }
    cells.push(agent(SubAgentActivityKind::Completed));
    let polling = crate::history_cell::ToolOutputGroup::from_cells([1, 3].map(|index| {
        Arc::new(
            ToolOutputCell::from_output(
                &format!("notification-{index}"),
                Some("one-call"),
                cases[index].0,
                /*namespace*/ None,
                &cases[index].1,
            )
            .unwrap(),
        ) as Arc<dyn HistoryCell>
    }));
    assert!(
        polling.display_lines(/*width*/ 100)[0]
            .to_string()
            .ends_with("Waited")
    );
    assert!(polling.activity_disclosure(/*width*/ 100).is_some());
    cells.push(polling);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 100, /*height*/ 22,
    );
    let mut buffer = Buffer::empty(area);
    cells.push(Arc::new(PlainHistoryCell::new(vec!["After tools".into()])));
    view.render(area, &mut buffer, &cells);
    let rendered = super::tests::text(&buffer).trim_end().to_owned();
    assert_eq!(rendered.matches("\n\n").count(), 2);
    insta::assert_snapshot!(rendered);

    // A following live tool must be as tightly spaced as its committed counterpart.
    cells.pop();
    let tail = ToolOutputCell::from_output(
        "tail",
        Some("tail"),
        "wait",
        /*namespace*/ None,
        &FunctionCallOutputBody::Text(running.into()),
    )
    .unwrap();
    let ids = tail.activity_ids();
    view.sync_live_activity(&cells, ids);
    view.sync_live_activity_tail(
        /*width*/ 100,
        /*key*/ None,
        /*expanded*/ false,
        |width, _, _| {
            Some(ActivityTranscriptLines {
                output_mode: None,
                is_tool_entry: tail.is_tool_entry(),
                header: tail.activity_header(),
                activity: tail.compact_hyperlink_lines(width),
                auxiliary: Vec::new(),
                header_tokens: None,
                disclosure: tail.activity_disclosure(width),
                controls: Vec::new(),
            })
        },
    );
    let mut live = Buffer::empty(area);
    view.render(area, &mut live, &cells);
    view.sync_live_tail(/*width*/ 100, /*key*/ None, |_| None);
    cells.push(Arc::new(tail));
    let mut committed = Buffer::empty(area);
    view.render(area, &mut committed, &cells);
    assert_eq!(live, committed);

    // Owned new/legacy first blocks disappear; identical script/notify text remains literal.
    let isolated = [3, 8, 9, 10].map(|index| {
        let id = format!("header-{index}");
        Arc::new(
            ToolOutputCell::from_output(
                &id,
                Some(&id),
                cases[index].0,
                /*namespace*/ None,
                &cases[index].1,
            )
            .unwrap(),
        ) as Arc<dyn HistoryCell>
    });
    let mut expanded_view = TranscriptView::default();
    for cell in &isolated {
        if cell.activity_disclosure(/*width*/ 100).is_some() {
            expanded_view
                .disclosure
                .expanded
                .extend(cell.activity_ids());
        }
    }
    let expanded = tests::render(
        &mut expanded_view,
        &isolated,
        /*width*/ 100,
        /*height*/ 24,
    );
    let text = tests::text(&expanded).trim_end().to_string();
    assert_eq!(text.matches("Script completed").count(), 1);
    assert_eq!(text.matches("Script failed").count(), 1);
    insta::assert_snapshot!("execution_headers_and_literal_text", text);
}
