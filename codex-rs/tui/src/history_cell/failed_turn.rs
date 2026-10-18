//! A failed turn's saved error and completion clock, independent of loaded item pages.

use super::FinalMessageSeparator;
use super::HistoryCell;
use super::PlainHistoryCell;
use super::new_error_event;
use crate::clock_format::ClockFormat;
use chrono::DateTime;
use chrono::Local;
use codex_app_server_protocol::CodexErrorInfo;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnError;
use codex_app_server_protocol::TurnStatus;
use ratatui::text::Line;

#[derive(Debug)]
pub(crate) struct FailedTurnCell {
    pub(crate) turn_id: String,
    pub(crate) error: TurnError,
    pub(crate) completed_at: Option<i64>,
    body: PlainHistoryCell,
    clock: Option<FinalMessageSeparator>,
}

impl FailedTurnCell {
    pub(crate) fn new(turn_id: String, error: TurnError, completed_at: Option<i64>) -> Self {
        let clock = completed_at
            .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0))
            .map(|timestamp| {
                FinalMessageSeparator::new(None, None)
                    .with_completed_at(timestamp.with_timezone(&Local), ClockFormat::system())
            });
        let body = new_error_event(error.message.clone());
        Self {
            turn_id,
            error,
            completed_at,
            body,
            clock,
        }
    }

    pub(crate) fn from_turn(turn: &Turn) -> Option<Self> {
        // Precautions have a dedicated continuation projection, not a terminal error.
        let error = turn.error.as_ref().filter(|error| {
            turn.status == TurnStatus::Failed
                && error.codex_error_info != Some(CodexErrorInfo::MisalignmentPolicyViolation)
        })?;
        Some(Self::new(turn.id.clone(), error.clone(), turn.completed_at))
    }
}

impl HistoryCell for FailedTurnCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = self.body.display_lines(width);
        if let Some(clock) = &self.clock {
            lines.extend(clock.display_lines(width));
        }
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = self.body.raw_lines();
        if let Some(clock) = &self.clock {
            lines.extend(clock.raw_lines());
        }
        lines
    }
}
