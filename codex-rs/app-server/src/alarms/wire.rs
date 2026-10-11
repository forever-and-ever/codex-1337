use chrono::DateTime;
use chrono::Local;
use codex_app_server_protocol::AlarmCondition;
use codex_app_server_protocol::AlarmSummary;
use codex_state::AlarmTrigger;
use codex_state::ThreadAlarm;
use codex_state::ThreadAlarmState;

pub(super) fn summary(alarm: &ThreadAlarm) -> Option<AlarmSummary> {
    let condition = match &alarm.state {
        ThreadAlarmState::Armed(AlarmTrigger::Clock { at }) => AlarmCondition::Clock { at: *at },
        ThreadAlarmState::Armed(AlarmTrigger::Watch {
            session_id,
            command,
            pattern,
            deadline,
        }) => AlarmCondition::Watch {
            session_id: *session_id,
            command: command.clone(),
            pattern: pattern.clone(),
            deadline: *deadline,
        },
        ThreadAlarmState::Sounded { .. } => return None,
    };
    Some(AlarmSummary {
        id: format!("a{}", alarm.id),
        armed_at: alarm.armed_at,
        note: alarm.note.clone(),
        condition,
    })
}

pub(super) fn time(at: i64) -> String {
    DateTime::from_timestamp_millis(at)
        .map(|at| at.with_timezone(&Local).to_rfc3339())
        .unwrap_or_else(|| at.to_string())
}

pub(super) fn condition(condition: &AlarmCondition) -> String {
    match condition {
        AlarmCondition::Clock { at } => format!("clock {}", time(*at)),
        AlarmCondition::Watch {
            session_id,
            command,
            pattern,
            deadline,
        } => {
            let mut text = format!("session {session_id} (`{command}`) exits");
            if let Some(pattern) = pattern {
                text.push_str(&format!(" or prints /{pattern}/"));
            }
            if let Some(deadline) = deadline {
                text.push_str(&format!(", or deadline {}", time(*deadline)));
            }
            text
        }
    }
}

pub(super) fn armed_list(alarms: &[AlarmSummary]) -> String {
    if alarms.is_empty() {
        return "Armed: none".into();
    }
    let rows = alarms
        .iter()
        .map(|alarm| {
            let note = alarm.note.chars().take(120).collect::<String>();
            format!(
                "{}: {}; armed {}; {note}",
                alarm.id,
                condition(&alarm.condition),
                time(alarm.armed_at)
            )
        })
        .collect::<Vec<_>>();
    format!("Armed:\n{}", rows.join("\n"))
}
