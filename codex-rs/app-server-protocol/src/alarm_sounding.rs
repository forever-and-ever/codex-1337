//! One canonical, readable body for unsolicited alarm results.

use chrono::DateTime;
use chrono::Local;
use codex_protocol::models::FunctionCallOutputBody;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlarmSounding {
    pub id: String,
    pub armed_at: i64,
    pub sounded_at: i64,
    pub note: String,
    pub cause: AlarmCause,
    pub still_armed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlarmCause {
    Clock {
        late_by_seconds: Option<u64>,
    },
    Exited {
        session_id: i32,
        command: String,
        code: Option<i32>,
        tail: String,
    },
    Matched {
        session_id: i32,
        command: String,
        line: String,
    },
    Deadline {
        session_id: i32,
        command: String,
    },
    Lost {
        session_id: i32,
        command: String,
    },
}

impl AlarmSounding {
    /// Callers also require an absent call ID: an ordinary tool response is not
    /// an unsolicited alarm sounding.
    pub fn from_tool_output(
        name: &str,
        namespace: Option<&str>,
        output: &FunctionCallOutputBody,
    ) -> Option<Self> {
        if namespace != Some("alarm") || name != "set" {
            return None;
        }
        Self::parse(output.to_text().as_deref()?)
    }

    pub fn render(&self) -> String {
        let (cause, detail) = match &self.cause {
            AlarmCause::Clock { late_by_seconds } => {
                let late = late_by_seconds
                    .map(|seconds| {
                        format!(
                            "; late by {}h{}m{}s; the server was not running",
                            seconds / 3600,
                            seconds / 60 % 60,
                            seconds % 60,
                        )
                    })
                    .unwrap_or_default();
                (format!("the set time came{late}."), String::new())
            }
            AlarmCause::Exited {
                session_id,
                command,
                code,
                tail,
            } => (
                format!(
                    "session {session_id} (`{}`) exited with code {}.",
                    escape_lines(command),
                    code.map(|code| code.to_string())
                        .unwrap_or_else(|| "signal/unknown".to_string())
                ),
                format!("\nLast output:\n{}", escape_lines(tail)),
            ),
            AlarmCause::Matched {
                session_id,
                command,
                line,
            } => (
                format!(
                    "session {session_id} (`{}`) matched the pattern.",
                    escape_lines(command)
                ),
                format!("\nMatched line:\n{}", escape_lines(line)),
            ),
            AlarmCause::Deadline {
                session_id,
                command,
            } => (
                format!(
                    "session {session_id} (`{}`) is still running; the deadline came.",
                    escape_lines(command)
                ),
                String::new(),
            ),
            AlarmCause::Lost {
                session_id,
                command,
            } => (
                format!(
                    "session {session_id} (`{}`) is no longer present.",
                    escape_lines(command)
                ),
                String::new(),
            ),
        };
        let armed = local_time(self.armed_at, "%H:%M:%S");
        let sounded = local_time(self.sounded_at, "%H:%M:%S");
        let offset = local_time(self.sounded_at, "%:z");
        let still_armed = if self.still_armed.is_empty() {
            "none".to_string()
        } else {
            self.still_armed.join(", ")
        };
        format!(
            "Alarm {} sounded: {cause}\nArmed {armed}, sounded {sounded} ({offset}).\n[alarm timestamps: {} {}]\nNote: {}{detail}\nStill armed: {still_armed}",
            self.id,
            self.armed_at,
            self.sounded_at,
            escape_lines(&self.note),
        )
    }

    pub fn parse(text: &str) -> Option<Self> {
        let (body, still_armed) = text.rsplit_once("\nStill armed: ")?;
        let still_armed = if still_armed == "none" {
            Vec::new()
        } else {
            still_armed.split(", ").map(str::to_string).collect()
        };
        let (header, payload) = body.split_once("\nNote: ")?;
        let (header, timestamps) = header.rsplit_once("\n[alarm timestamps: ")?;
        let (armed_at, sounded_at) = timestamps.strip_suffix(']')?.split_once(' ')?;
        let armed_at = armed_at.parse().ok()?;
        let sounded_at = sounded_at.parse().ok()?;
        let (header, _) = header.rsplit_once("\nArmed ")?;
        let (id, description) = header.strip_prefix("Alarm ")?.split_once(" sounded: ")?;
        let (cause, note) = if description == "the set time came." {
            (
                AlarmCause::Clock {
                    late_by_seconds: None,
                },
                payload,
            )
        } else if let Some(late) = description.strip_prefix("the set time came; late by ") {
            let duration = late.strip_suffix("; the server was not running.")?;
            let (hours, duration) = duration.split_once('h')?;
            let (minutes, seconds) = duration.split_once('m')?;
            let hours: u64 = hours.parse().ok()?;
            let minutes: u64 = minutes.parse().ok()?;
            let seconds: u64 = seconds.strip_suffix('s')?.parse().ok()?;
            let seconds = hours
                .checked_mul(3600)?
                .checked_add(minutes.checked_mul(60)?)?
                .checked_add(seconds)?;
            (
                AlarmCause::Clock {
                    late_by_seconds: Some(seconds),
                },
                payload,
            )
        } else {
            let (session_id, rest) = description.strip_prefix("session ")?.split_once(" (`")?;
            let session_id = session_id.parse().ok()?;
            let (command, reason) = rest.rsplit_once("`) ")?;
            let command = unescape_lines(command);
            if let Some(code) = reason.strip_prefix("exited with code ") {
                let code = code.strip_suffix('.')?;
                let code = if code == "signal/unknown" {
                    None
                } else {
                    Some(code.parse().ok()?)
                };
                let (note, tail) = payload.split_once("\nLast output:\n")?;
                (
                    AlarmCause::Exited {
                        session_id,
                        command,
                        code,
                        tail: unescape_lines(tail),
                    },
                    note,
                )
            } else {
                match reason {
                    "matched the pattern." => {
                        let (note, line) = payload.split_once("\nMatched line:\n")?;
                        (
                            AlarmCause::Matched {
                                session_id,
                                command,
                                line: unescape_lines(line),
                            },
                            note,
                        )
                    }
                    "is still running; the deadline came." => (
                        AlarmCause::Deadline {
                            session_id,
                            command,
                        },
                        payload,
                    ),
                    "is no longer present." => (
                        AlarmCause::Lost {
                            session_id,
                            command,
                        },
                        payload,
                    ),
                    _ => return None,
                }
            }
        };
        Some(Self {
            id: id.to_string(),
            armed_at,
            sounded_at,
            note: unescape_lines(note),
            cause,
            still_armed,
        })
    }
}

fn local_time(millis: i64, format: &str) -> String {
    DateTime::from_timestamp_millis(millis)
        .map(|time| time.with_timezone(&Local).format(format).to_string())
        .unwrap_or_else(|| "out of range".to_string())
}

// A backslash protects only marker-like lines and existing leading backslashes;
// ordinary prose, shell syntax and XML-looking output remain verbatim.
fn escape_lines(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            if [
                "\\",
                "Note:",
                "Last output:",
                "Matched line:",
                "Still armed:",
                "Armed ",
                "[alarm timestamps:",
            ]
            .iter()
            .any(|marker| line.starts_with(marker))
            {
                format!("\\{line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn unescape_lines(text: &str) -> String {
    text.split('\n')
        .map(|line| line.strip_prefix('\\').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "alarm_sounding_tests.rs"]
mod tests;
