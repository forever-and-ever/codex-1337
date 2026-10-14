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
}
