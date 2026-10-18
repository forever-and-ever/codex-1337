//! Alarm tails use the command-output row budget and transcript disclosure hint.
use super::CommandOutput;
use super::ExecCell;
use super::Line;
use super::OutputLinesParams;
use super::TOOL_CALL_MAX_LINES;
use super::output_lines;
use super::plain_hyperlink_lines;
use super::visible_lines;

pub(crate) fn alarm_output_lines(output: &CommandOutput, width: u16) -> Vec<Line<'static>> {
    let output = output_lines(
        Some(output),
        OutputLinesParams {
            line_limit: TOOL_CALL_MAX_LINES,
            only_err: false,
            include_angle_pipe: true,
            include_prefix: true,
        },
    );
    let lines = plain_hyperlink_lines(output.lines);
    visible_lines(ExecCell::truncate_lines_middle(
        &lines,
        TOOL_CALL_MAX_LINES,
        width,
        output.omitted,
        /*ellipsis_prefix*/ None,
    ))
}
