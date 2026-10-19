//! Shared presentation of authoritative thread status. Absence is not liveness.
use codex_app_server_protocol::ThreadStatus;
use ratatui::style::Stylize;
use ratatui::text::Span;

pub(crate) fn marker(status: Option<&ThreadStatus>) -> Span<'static> {
    match status {
        // The 1337 status contract distinguishes input awaited from failure.
        Some(ThreadStatus::Active { active_flags }) if !active_flags.is_empty() => {
            "■".fg(crate::warpaint::Figure::Caution.color())
        }
        Some(ThreadStatus::Active { .. }) => "●".green(),
        Some(ThreadStatus::SystemError) => "■".red(),
        Some(ThreadStatus::Idle) => "■".into(),
        Some(ThreadStatus::NotLoaded) => "△".dim(),
        None => "?".dim(),
    }
}
