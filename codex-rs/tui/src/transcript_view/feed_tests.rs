//! The feed's one interaction worth pinning: steps fold under their headers with their figures,
//! a Code Mode header shows its block's script, and an activity whose only row became its
//! header keeps its details behind its own control.

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
    let fact = |suffix: &str, event| CodeModeItem {
        id: format!("code-mode:{exec}:{suffix}"),
        cell_id: None,
        exec_call_id: exec.into(),
        created_at: CLOCK_MS / 1_000,
        event,
    };
    let mut block = CodeModeCell::owned(
        CodeModeOwner {
            thread_id: ThreadId::from_u128(/*value*/ 7),
            exec_call_id: exec.into(),
            cell_id: None,
            started_at_ms: CLOCK_MS,
        },
        /*animations_enabled*/ false,
    );
    block.admit_fact(
        fact(
            "start",
            CodeModeEvent::Started {
                call_id: exec.into(),
                script: Some(
                    "const status = await tools.exec_command({ cmd: \"git status\" });\ntext(status.output);\n"
                        .into(),
                ),
            },
        ),
        /*live*/ false,
    );
    let mut status = new_active_exec_command(
        "call-status".into(),
        ["git", "status"].map(String::from).to_vec(),
        Vec::new(),
        CommandExecutionSource::Agent,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    status.complete_call(
        "call-status",
        CommandOutput::new(/*exit_code*/ 0, "clean\n".into()),
        std::time::Duration::ZERO,
    );
    block.admit_children("call-status".into(), vec![Arc::new(status)]);
    block.admit_fact(
        fact(
            "end",
            CodeModeEvent::Finished {
                status: CodeModeStatus::Completed,
            },
        ),
        /*live*/ false,
    );
    Arc::new(block)
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
            id: "arrival:resp-1:0".into(),
            response_id: "resp-1".into(),
            announced_at_ms: CLOCK_MS,
            kind: ArrivalKind::Injection,
            label: "environment_context".into(),
            text: Some("<environment_context>cwd</environment_context>".into()),
        })),
        message("msg-2", "Fixed the parser."),
        thought("resp-2"),
        Arc::new(UserHistoryCell {
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
}
