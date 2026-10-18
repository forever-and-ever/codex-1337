use chrono::DateTime;
use chrono::Local;
use codex_app_server_protocol::AlarmCondition;
use codex_app_server_protocol::AlarmSummary;
use ratatui::style::Stylize;
use ratatui::text::Line;

pub(crate) fn alarm_label(alarm: &AlarmSummary) -> String {
    match &alarm.condition {
        AlarmCondition::Clock { at } => clock_label(*at),
        AlarmCondition::Watch { session_id, .. } => format!("#{session_id}"),
    }
}

fn clock_label(at: i64) -> String {
    DateTime::from_timestamp_millis(at).map_or_else(|| at.to_string(), |time| {
            let time = time.with_timezone(&Local);
            let format = if time.date_naive() == Local::now().date_naive() {
                "%H:%M"
            } else {
                "%m-%d %H:%M"
            };
            time.format(format).to_string()
        })
}

pub(crate) fn alarm_detail(alarm: &AlarmSummary) -> String {
    match &alarm.condition {
        AlarmCondition::Clock { at } => clock_label(*at),
        AlarmCondition::Watch {
            session_id,
            command,
            pattern,
            deadline,
        } => {
            let mut detail = format!("#{session_id} {command} · exits");
            if let Some(pattern) = pattern {
                detail.push_str(&format!(" or /{pattern}/"));
            }
            if let Some(deadline) = deadline {
                detail.push_str(&format!(" · by {}", clock_label(*deadline)));
            }
            detail
        }
    }
}

pub(super) fn line(alarms: &[AlarmSummary]) -> Option<Line<'static>> {
    let alarm = alarms.first()?;
    let mut label = format!("◷ {}", alarm_label(alarm));
    if alarms.len() > 1 {
        label.push_str(&format!(" +{}", alarms.len() - 1));
    }
    Some(Line::from(label.cyan()))
}
