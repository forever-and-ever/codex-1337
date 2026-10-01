//! Grouped command details retain terminal input in raw output without exposing hidden reasoning.

use super::*;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::model::ExecCall;
use crate::history_cell::HistoryCell;
use crate::history_cell::new_reasoning_summary_block;
use crate::history_cell::new_unified_exec_interaction;
use codex_app_server_protocol::CommandExecutionSource;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn completed_read(name: &str, output: &str) -> ExecCell {
    let command = vec!["cat".to_owned(), name.to_owned()];
    let parsed = codex_shell_command::parse_command::parse_command(&command);
    ExecCell::new(
        ExecCall {
            rows: Default::default(),
            call_id: name.to_owned(),
            command,
            parsed,
            output: Some(CommandOutput::new(/*exit_code*/ 0, output.to_owned())),
            source: CommandExecutionSource::UnifiedExecStartup,
            start_time: None,
            duration: Some(Duration::from_millis(/*millis*/ 5)),
            interaction_input: None,
        },
        /*animations_enabled*/ false,
    )
}

#[test]
fn raw_grouped_history_retains_terminal_input_and_omits_reasoning() {
    let mut group = completed_read("first.txt", "first output");
    group
        .group
        .push_detail(Arc::from(new_reasoning_summary_block(
            vec!["Inspecting the next file".to_owned()],
            Path::new("."),
        ) as Box<dyn HistoryCell>));
    group
        .group
        .push_detail(Arc::new(new_unified_exec_interaction(
            Some("cat first.txt".to_owned()),
            "continue\n".to_owned(),
        )));

    let raw = group
        .raw_lines()
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>();
    let rich = group
        .transcript_lines(/*width*/ 80)
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(format!("Rich\n{rich}\n\nRaw\n{}", raw.join("\n")));
}
