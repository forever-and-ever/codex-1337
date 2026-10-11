//! A sounded alarm is immutable transcript output, not a user message.
use super::*;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::alarm_output_lines;
use codex_app_server_protocol::alarm_sounding::AlarmCause;
use codex_app_server_protocol::alarm_sounding::AlarmSounding;

#[derive(Debug)]
pub(crate) struct AlarmSoundingCell(pub(crate) AlarmSounding);

impl AlarmSoundingCell {
    fn lines(&self, tail: Vec<Line<'static>>) -> Vec<Line<'static>> {
        let alarm = &self.0;
        let cause = match &alarm.cause {
            AlarmCause::Clock { late_by_seconds } => late_by_seconds.map_or_else(
                || "time reached".into(),
                |seconds| format!("time reached · {seconds}s late"),
            ),
            AlarmCause::Exited {
                session_id, code, ..
            } => {
                let code = code.map_or_else(|| "signal/unknown".into(), |code| code.to_string());
                format!("session #{session_id} exited {code}")
            }
            AlarmCause::Matched { session_id, .. } => format!("session #{session_id} matched"),
            AlarmCause::Deadline { session_id, .. } => {
                format!("session #{session_id} deadline reached")
            }
            AlarmCause::Lost { session_id, .. } => format!("session #{session_id} lost"),
        };
        let heading =
            sanitize_user_text(format!("Alarm {} · {cause}", alarm.id).into()).into_owned();
        let note = sanitize_user_text(alarm.note.as_str().into());
        let mut lines = vec![Line::from(heading.bold())];
        lines.extend(note.lines().map(|line| Line::from(line.to_owned().dim())));
        match &alarm.cause {
            AlarmCause::Clock {
                late_by_seconds: Some(_),
            } => lines.push("The server was not running.".dim().into()),
            AlarmCause::Clock {
                late_by_seconds: None,
            } => {}
            AlarmCause::Exited { command, .. }
            | AlarmCause::Matched { command, .. }
            | AlarmCause::Deadline { command, .. }
            | AlarmCause::Lost { command, .. } => {
                let command = sanitize_user_text(command.as_str().into());
                lines.extend(
                    command
                        .lines()
                        .map(|line| Line::from(line.to_owned().dim())),
                );
            }
        }
        match &alarm.cause {
            AlarmCause::Exited { .. } => lines.extend(tail),
            AlarmCause::Matched { line, .. } => {
                let line = sanitize_user_text(line.as_str().into());
                lines.extend(line.lines().map(|line| Line::from(line.to_owned())));
            }
            AlarmCause::Deadline { .. } => lines.push("Still running.".into()),
            AlarmCause::Lost { .. } => lines.push("Session is no longer present.".into()),
            AlarmCause::Clock { .. } => {}
        }
        if !alarm.still_armed.is_empty() {
            let armed =
                sanitize_user_text(format!("Still armed: {}", alarm.still_armed.join(", ")).into())
                    .into_owned();
            lines.push(armed.dim().into());
        }
        lines
    }

    fn full_lines(&self) -> Vec<Line<'static>> {
        let tail = match &self.0.cause {
            AlarmCause::Exited { tail, .. } => sanitize_user_text(tail.as_str().into())
                .lines()
                .map(|line| Line::from(line.to_owned()))
                .collect(),
            _ => Vec::new(),
        };
        self.lines(tail)
    }
}

impl HistoryCell for AlarmSoundingCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        HistoryTimestamp::from_millis(self.0.sounded_at)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let tail = match &self.0.cause {
            AlarmCause::Exited { tail, code, .. } => {
                let output = CommandOutput::new(
                    code.unwrap_or(1),
                    sanitize_user_text(tail.as_str().into()).into_owned(),
                );
                alarm_output_lines(&output, width.saturating_sub(2).max(1))
            }
            _ => Vec::new(),
        };
        prefix_lines(
            adaptive_wrap_lines(
                self.lines(tail),
                RtOptions::new(width.saturating_sub(2).max(1) as usize),
            ),
            "◷ ".into(),
            "  ".into(),
        )
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.full_lines()
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        prefix_lines(
            adaptive_wrap_lines(
                self.full_lines(),
                RtOptions::new(width.saturating_sub(2).max(1) as usize),
            ),
            "◷ ".into(),
            "  ".into(),
        )
    }
}

#[cfg(test)]
#[path = "alarm_tests.rs"]
mod tests;
