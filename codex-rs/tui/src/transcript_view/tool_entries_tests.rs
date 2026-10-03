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
            |width, _| {
                Some(ActivityTranscriptLines {
                    is_tool_entry: false,
                    header: None,
                    activity: Vec::new(),
                    auxiliary: tail.compact_hyperlink_lines(width),
                    tool_output_tokens: None,
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
fn waits_and_agent_events_keep_status_without_empty_disclosures_or_extra_gaps() {
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
        assert_eq!(
            cell.activity_disclosure(/*width*/ 100).is_some(),
            index == cases.len() - 1
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
            .ends_with("finished, exit 0")
    );
    assert!(polling.activity_disclosure(/*width*/ 100).is_none());
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
        |width, _| {
            Some(ActivityTranscriptLines {
                is_tool_entry: tail.is_tool_entry(),
                header: tail.activity_header(),
                activity: tail.compact_hyperlink_lines(width),
                auxiliary: Vec::new(),
                tool_output_tokens: None,
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
}
