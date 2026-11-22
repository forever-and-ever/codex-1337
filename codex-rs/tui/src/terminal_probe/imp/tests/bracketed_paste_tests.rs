//! DECRPM replies share the existing startup probe round trip.

use super::*;
use pretty_assertions::assert_eq;

pub(super) fn batched_terminal_responses() {
    for (reply, bracketed_paste) in [
        ("\x1b[?2004;1$y", Some(true)),
        ("\x1b[?2004;3$y", Some(true)),
        ("\x1b[?2004;2$y", Some(false)),
        ("\x1b[?2004;4$y", Some(false)),
        ("\x1b[?2004;0$y", Some(false)),
        ("", None),
    ] {
        let mut probe = StartupProbe {
            cursor_position: None,
            default_colors: None,
            keyboard_enhancement_supported: None,
            terminal_app_over_ssh: Some(false),
            bracketed_paste: None,
            alternate_scroll_enabled: None,
        };
        let mut saw_supported_keyboard = false;
        let mut buffer = reply.as_bytes().to_vec();
        buffer.extend_from_slice(b"\x1B[20;10R\x1B]11;rgb:1111/1111/1111\x07\x1B[?64;1;2c\x1B]10;rgb:eeee/eeee/eeee\x1B\\\x1B[?7u");
        update_startup_probe(
            &mut probe,
            &mut saw_supported_keyboard,
            &buffer,
            StartupKeyboardEnhancementProbe::Query,
        );
        assert_eq!(
            probe,
            StartupProbe {
                cursor_position: Some(Position { x: 9, y: 19 }),
                default_colors: Some(DefaultColors {
                    fg: (238, 238, 238),
                    bg: (17, 17, 17),
                }),
                keyboard_enhancement_supported: Some(true),
                terminal_app_over_ssh: Some(false),
                bracketed_paste,
                alternate_scroll_enabled: None,
            }
        );
        // Missing DECRPM must not extend the existing round trip.
        assert!(startup_probe_complete(
            &probe,
            StartupKeyboardEnhancementProbe::Query
        ));
        let typed = format!("before{reply}after");
        assert_eq!(startup_replay_input(typed.as_bytes()), b"beforeafter");
    }
}
