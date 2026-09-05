use super::*;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::new_active_exec_command;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::streaming::controller::StreamController;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use codex_app_server_protocol::CommandExecutionSource;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use std::path::Path;
use std::time::Duration;

#[test]
fn timestamp_gutter_survives_stream_finalization_and_reflow() {
    // The stream and source-backed render must agree on the clock gutter, especially for tables.
    let timestamp = HistoryTimestamp(NaiveTime::from_hms_opt(13, 4, 5).unwrap());
    let source = "A **reply** with a table and enough prose to wrap.\n\n| Name | Value |\n| --- | --- |\n| alpha | 42 |\n";
    let mut stream = StreamController::new(Some(49), Path::new("/tmp"), HistoryRenderMode::Rich);
    stream.timestamp = Some(timestamp);
    stream.push(source);
    let (streamed, _) = stream.finalize();
    let streamed = streamed.unwrap();
    let mut completed = AgentMarkdownCell::new(source.to_string(), Path::new("/tmp"));
    completed.timestamp = streamed
        .as_any()
        .downcast_ref::<crate::history_cell::AgentMessageCell>()
        .unwrap()
        .timestamp;
    let render = |cell: &dyn HistoryCell| {
        let area = Rect::new(0, 0, 60, 12);
        let mut buffer = Buffer::empty(area);
        Paragraph::new(cell.display_lines(area.width)).render(area, &mut buffer);
        buffer
    };
    assert_eq!(render(streamed.as_ref()), render(&completed));
    assert_eq!(
        completed.raw_lines(),
        AgentMarkdownCell::new(source.to_string(), Path::new("/tmp")).raw_lines()
    );

    let mut command = new_active_exec_command(
        "call".into(),
        vec!["echo".into(), "hello".into()],
        vec![],
        CommandExecutionSource::Agent,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    command.completed_at = Some(timestamp);
    assert!(
        !command.display_lines(60)[0]
            .to_string()
            .contains("13:04:05")
    );
    command.complete_call(
        "call",
        CommandOutput::new(0, "hello".into()),
        Duration::from_secs(1),
    );

    let mut snapshot = String::new();
    for width in [60, 32, 60] {
        snapshot.push_str(&format!("width {width}\n"));
        for line in plain_hyperlink_lines(command.display_lines(width))
            .into_iter()
            .chain(completed.display_hyperlink_lines(width))
        {
            assert!(line.line.width() <= usize::from(width));
            snapshot.push_str(&line.line.to_string());
            snapshot.push('\n');
        }
    }
    insta::assert_snapshot!(snapshot);
}
