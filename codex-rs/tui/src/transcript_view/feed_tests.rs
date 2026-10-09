//! The feed's one interaction worth pinning: steps fold under their headers with their figures.

use super::*;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::new_active_exec_command;
use crate::history_cell::ThoughtTextVisibility;
use codex_app_server_protocol::ArrivalItem;
use codex_app_server_protocol::CommandExecutionSource;
use codex_app_server_protocol::ThreadItem;
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
        vec!["cargo".into(), "test".into()],
        Vec::new(),
        CommandExecutionSource::Agent,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    exec.complete_call(
        "call-1",
        CommandOutput::new(/*exit_code*/ 1, "test parse ... FAILED\n".into()),
        std::time::Duration::ZERO,
    );
    let cells: Vec<Arc<dyn HistoryCell>> = vec![
        Arc::new(UserHistoryCell {
            message: "Fix the failing test".into(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
            spoken: false,
        }),
        message("msg-1", "I'll run the tests first."),
        thought("resp-1"),
        Arc::new(exec),
        Arc::new(ArrivalCell::new(ArrivalItem {
            id: "arrival:resp-1:0".into(),
            response_id: "resp-1".into(),
            announced_at_ms: CLOCK_MS,
            kind: ArrivalKind::Injection,
            label: "environment_context".into(),
            text: Some("<environment_context>cwd</environment_context>".into()),
        })),
        thought("resp-2"),
        message("msg-2", "Fixed the parser."),
        Arc::new(UserHistoryCell {
            message: "Thanks".into(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
            spoken: false,
        }),
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
    let clock = crate::history_cell::HistoryTimestamp::from_millis(CLOCK_MS)
        .unwrap()
        .span()
        .content
        .trim()
        .to_owned();
    let mut snapshots = Vec::new();
    let mut screen = |view: &mut TranscriptView, label: &str| {
        let buffer = crate::transcript_view::tests::render(
            view, &cells, /*width*/ 96, /*height*/ 22,
        );
        let text = crate::transcript_view::tests::text(&buffer).replace(&clock, "HH:MM");
        snapshots.push(format!("{label}\n{text}"));
        text
    };
    let folded = screen(&mut view, "folded");
    let row = folded
        .lines()
        .position(|line| line.contains("cargo test"))
        .unwrap_or_else(|| panic!("the step's header names its call:\n{folded}"));
    assert!(view.toggle_disclosure_at(
        &cells,
        crate::transcript_geometry::FOLD_COLUMN + 1,
        row as u16,
    ));
    screen(&mut view, "unfolded");
    insta::assert_snapshot!(snapshots.join("\n\n"));
}
