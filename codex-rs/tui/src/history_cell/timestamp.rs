//! Local wall-clock labels captured from app-server item lifecycle events.

use chrono::DateTime;
use chrono::Local;
use chrono::NaiveTime;
use ratatui::style::Stylize;
use ratatui::text::Span;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HistoryTimestamp(NaiveTime);

impl HistoryTimestamp {
    pub(crate) fn from_millis(timestamp: i64) -> Option<Self> {
        // Zero is used by synthetic notifications that have no event clock.
        (timestamp > 0)
            .then(|| DateTime::from_timestamp_millis(timestamp))
            .flatten()
            .map(|time| Self(time.with_timezone(&Local).time()))
    }

    pub(crate) fn span(self) -> Span<'static> {
        format!("{} ", self.0.format("%H:%M:%S")).reset().dim()
    }
}

pub(crate) fn agent_prefix(timestamp: Option<HistoryTimestamp>, first: bool) -> Span<'static> {
    match (timestamp, first) {
        (Some(timestamp), true) => format!("• {} ", timestamp.0.format("%H:%M:%S"))
            .reset()
            .dim(),
        (None, true) => "• ".dim(),
        (_, false) => "  ".into(),
    }
}

#[cfg(test)]
#[path = "timestamp_tests.rs"]
mod tests;
