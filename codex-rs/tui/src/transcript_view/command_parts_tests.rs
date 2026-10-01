use super::*;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::new_active_exec_command;
use crate::history_cell::activity_parts::FoldTarget;
use codex_app_server_protocol::CommandExecutionSource;
use codex_shell_command::parse_command::parse_command;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use std::time::Duration;

#[test]
fn shell_rows_share_output_but_have_independent_source_folds() {
    let script = "sed -n '10,20p' src/main.rs; git diff --stat --ignore-all-space -- src/very_long_directory_name && echo 'not;a&&boundary'";
    let command = vec!["bash".into(), "-lc".into(), script.into()];
    let mut cell = new_active_exec_command(
        "train".into(),
        command.clone(),
        parse_command(&command),
        CommandExecutionSource::UnifiedExecStartup,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    cell.complete_call(
        "train",
        CommandOutput::new(/*exit_code*/ 0, "combined output".into()),
        Duration::ZERO,
    );
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(cell)];
    let mut view = TranscriptView::default();
    view.record_tool_output_tokens("bill".into(), "train".into(), Some(2352));
    let mut snapshots = Vec::new();
    let mut render = |view: &mut TranscriptView, label: &str, width: u16| {
        view.position = Position::Latest;
        view.release_live_reading();
        let area = Rect::new(/*x*/ 0, /*y*/ 0, width, /*height*/ 24);
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer, &cells);
        snapshots.push(format!("{label}\n{}", tests::text(&buffer).trim_end()));
    };
    render(&mut view, "collapsed", 64);
    let targets = view
        .current_layout(&cells, /*index*/ 0)
        .unwrap()
        .fold_targets();
    assert_eq!(targets.len(), 3);
    assert_eq!(targets[0], FoldTarget::Activity);
    assert!(view.toggle_disclosure_at(&cells, /*column*/ 0, /*row*/ 0));
    render(&mut view, "output only", 64);
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    for code in [KeyCode::Down, KeyCode::Enter] {
        view.handle_key(key(code), &cells);
    }
    render(&mut view, "read command expanded", 64);
    for code in [KeyCode::Down, KeyCode::Enter, KeyCode::Down, KeyCode::Down] {
        view.handle_key(key(code), &cells);
    }
    assert_eq!(view.disclosure.focused.as_ref().unwrap().target, targets[2]);
    render(&mut view, "both command folds expanded", 64);
    let layout = view.current_layout(&cells, /*index*/ 0).unwrap();
    assert!(
        layout
            .text()
            .contains("git diff --stat --ignore-all-space -- src/very_long_directory_name &&")
    );
    assert_eq!(layout.text().matches("combined output").count(), 1);
    render(&mut view, "narrow", 40);
    render(&mut view, "wide", 110);
    for code in [KeyCode::Up, KeyCode::Up, KeyCode::Up] {
        view.handle_key(key(code), &cells);
    }
    assert_eq!(view.disclosure.focused.as_ref().unwrap().target, targets[0]);
    view.handle_key(key(KeyCode::Enter), &cells);
    render(&mut view, "commands open, output closed", 64);
    assert!(
        !view
            .current_layout(&cells, /*index*/ 0)
            .unwrap()
            .text()
            .contains("combined output")
    );
    // Live and committed entries consume the same command-row projection and fold keys.
    let expected = view
        .current_layout(&cells, /*index*/ 0)
        .unwrap()
        .text()
        .to_owned();
    view.sync_live_activity(&[], cells[0].activity_ids());
    view.sync_live_activity_tail(
        /*width*/ 64,
        /*key*/ None,
        /*expanded*/ false,
        |width, expanded| {
            let parts = cells[0]
                .activity_parts(width - 4, /*header_reserve*/ 17, expanded)
                .unwrap();
            Some(ActivityTranscriptLines {
                activity: parts.lines,
                auxiliary: Vec::new(),
                tool_output_tokens: Some(2352),
                disclosure: parts.output,
                controls: parts.controls,
            })
        },
    );
    assert_eq!(view.live.as_ref().unwrap().text(), expected);
    insta::assert_snapshot!(snapshots.join("\n\n"));
}
