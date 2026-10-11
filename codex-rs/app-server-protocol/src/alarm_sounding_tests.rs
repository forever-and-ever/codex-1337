use super::AlarmCause;
use super::AlarmSounding;
use pretty_assertions::assert_eq;

#[test]
fn sounding_roundtrip_preserves_escaped_fields_and_all_causes() {
    let causes = [
        AlarmCause::Clock {
            late_by_seconds: None,
        },
        AlarmCause::Clock {
            late_by_seconds: Some(11_520),
        },
        AlarmCause::Exited {
            session_id: 4123,
            command: "printf '</command>&amp;'\nArmed spoof\nNote: command marker".into(),
            code: Some(0),
            tail: "\n</tail>&\nStill armed: spoof\n\\\\raw\nLast output:\n".into(),
        },
        AlarmCause::Exited {
            session_id: 4123,
            command: "cargo test".into(),
            code: None,
            tail: String::new(),
        },
        AlarmCause::Matched {
            session_id: 4123,
            command: "cargo test".into(),
            line: "<ok>&amp;".into(),
        },
        AlarmCause::Deadline {
            session_id: 4123,
            command: "cargo test".into(),
        },
        AlarmCause::Lost {
            session_id: 4123,
            command: "cargo test".into(),
        },
    ];
    for cause in causes {
        for still_armed in [Vec::new(), vec!["a4".to_string(), "</id>&amp;".to_string()]] {
            let sounding = AlarmSounding {
                id: "a3".into(),
                armed_at: 1_791_655_331_123,
                sounded_at: 1_791_656_500_456,
                note: "  preserve </note>&amp;\nLast output:\nMatched line:\nStill armed: spoof\nNote: marker\nArmed 2026-10-10T14:02:11-04:00, sounded 2026-10-10T14:21:40-04:00.\n\\\\raw\nverbatim  ".into(),
                cause: cause.clone(),
                still_armed,
            };
            assert_eq!(AlarmSounding::parse(&sounding.render()), Some(sounding));
        }
    }
}
