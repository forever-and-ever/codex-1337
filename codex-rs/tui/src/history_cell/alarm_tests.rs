use super::*;
use codex_app_server_protocol::alarm_sounding::AlarmCause;

#[test]
fn sounded_alarm_transcript() {
    let causes = [
        AlarmCause::Clock {
            late_by_seconds: Some(12),
        },
        AlarmCause::Exited {
            session_id: 27,
            command: "just test".into(),
            code: Some(1),
            tail: (1..=14)
                .map(|line| format!("output line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        },
        AlarmCause::Matched {
            session_id: 27,
            command: "just test".into(),
            line: "test result: ok".into(),
        },
        AlarmCause::Deadline {
            session_id: 27,
            command: "just test".into(),
        },
        AlarmCause::Lost {
            session_id: 27,
            command: "just test".into(),
        },
    ];
    let rendered = causes
        .into_iter()
        .map(|cause| {
            let cell = AlarmSoundingCell(AlarmSounding {
                id: "check-tests".into(),
                armed_at: 1_000,
                sounded_at: 2_000,
                note: "Check test results".into(),
                cause,
                still_armed: vec!["follow-up".into()],
            });
            let lines = cell.display_lines(62);
            let styles = format!(
                "Header: {:?}; note: {:?}",
                lines[0].spans.last().unwrap().style.add_modifier,
                lines[1].spans.last().unwrap().style.add_modifier
            );
            let compact = format!(
                "{}\n{styles}",
                lines
                    .iter()
                    .map(Line::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            if matches!(cell.0.cause, AlarmCause::Exited { .. }) {
                format!(
                    "{compact}\nExpanded:\n{}",
                    cell.transcript_lines(62)
                        .iter()
                        .map(Line::to_string)
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            } else {
                compact
            }
        })
        .collect::<Vec<_>>()
        .join("\n---\n");
    insta::assert_snapshot!(rendered);
}
