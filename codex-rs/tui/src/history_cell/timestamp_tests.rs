use super::*;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::new_active_exec_command;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::streaming::controller::StreamController;
use codex_app_server_protocol::CommandExecutionSource;
use codex_protocol::parse_command::ParsedCommand;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use std::path::Path;
use std::time::Duration;

#[test]
fn timestamp_gutter_survives_stream_finalization_and_reflow() {
    // First-line clocks and normal continuation indents must agree across stream finalization.
    let timestamp = HistoryTimestamp(NaiveTime::from_hms_opt(13, 4, 5).unwrap());
    let source = "A **reply** with a table and enough prose to wrap.\n\n| Name | Value |\n| --- | --- |\n| alpha | 42 |\n";
    let mut stream = StreamController::new(Some(48), Path::new("/tmp"), HistoryRenderMode::Rich);
    stream.timestamp = Some(timestamp);
    let _ = stream.push(source);
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
        let width = crate::transcript_geometry::content_width(area.width, HistoryRenderMode::Rich);
        let lines =
            crate::transcript_geometry::rich_lines(crate::transcript_geometry::clock_lines(
                cell.display_hyperlink_lines(width),
                cell.transcript_timestamp(),
            ));
        Paragraph::new(crate::terminal_hyperlinks::visible_lines(lines)).render(area, &mut buffer);
        buffer
    };
    assert_eq!(render(streamed.as_ref()), render(&completed));
    let mut prompt = crate::history_cell::new_user_prompt(
        "A user prompt with enough words to wrap across the viewport.".into(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let raw_prompt = prompt.raw_lines();
    prompt.timestamp = Some(timestamp);
    let painted_prompt = render(&prompt);
    assert_eq!(
        (0..crate::transcript_geometry::CLOCK_COLUMNS)
            .map(|column| painted_prompt[(column, 0)].symbol())
            .collect::<String>(),
        "  13:04 ",
    );
    assert_eq!(prompt.raw_lines(), raw_prompt);
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
    assert_eq!(command.transcript_timestamp(), None);
    let _ = command.complete_call(
        "call",
        CommandOutput::new(/*exit_code*/ 0, "hello".into()),
        Duration::from_secs(1),
    );

    let mut exploration = new_active_exec_command(
        "read".into(),
        vec!["cat".into(), "src/lib.rs".into()],
        vec![ParsedCommand::Read {
            name: "lib.rs".into(),
            cmd: "cat src/lib.rs".into(),
            path: "src/lib.rs".into(),
        }],
        CommandExecutionSource::Agent,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    exploration.completed_at = Some(timestamp);
    let _ = exploration.complete_call("read", CommandOutput::default(), Duration::from_secs(1));

    let mut snapshot = String::new();
    for width in [60, 32, 60] {
        snapshot.push_str("width ");
        snapshot.push_str(&width.to_string());
        snapshot.push('\n');
        let content_width =
            crate::transcript_geometry::content_width(width, HistoryRenderMode::Rich);
        let lines = crate::transcript_geometry::clock_lines(
            command.display_hyperlink_lines(content_width),
            command.transcript_timestamp(),
        )
        .into_iter()
        .chain(crate::transcript_geometry::clock_lines(
            exploration.display_hyperlink_lines(content_width),
            exploration.transcript_timestamp(),
        ))
        .chain(crate::transcript_geometry::clock_lines(
            completed.display_hyperlink_lines(content_width),
            completed.transcript_timestamp(),
        ))
        .collect();
        for line in crate::transcript_geometry::rich_lines(lines) {
            assert!(line.line.width() <= usize::from(width));
            snapshot.push_str(line.line.to_string().trim_end());
            snapshot.push('\n');
        }
    }
    insta::assert_snapshot!(snapshot);
}
