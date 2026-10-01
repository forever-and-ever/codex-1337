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
