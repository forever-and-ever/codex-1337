//! Shared presentation of authoritative thread status. Absence is not liveness.
use codex_app_server_protocol::ThreadStatus;
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Span;

#[expect(
    clippy::disallowed_methods,
    reason = "The house status palette requires orange for input."
)]
pub(crate) fn marker(status: Option<&ThreadStatus>) -> Span<'static> {
    match status {
        // The 1337 status contract distinguishes orange input from red failure.
        Some(ThreadStatus::Active { active_flags }) if !active_flags.is_empty() => {
            "■".fg(Color::Indexed(208))
        }
        Some(ThreadStatus::Active { .. }) => "●".green(),
        Some(ThreadStatus::SystemError) => "■".red(),
        Some(ThreadStatus::Idle) => "■".into(),
        Some(ThreadStatus::NotLoaded) => "△".dim(),
        None => "?".dim(),
    }
}
