//! Async questions and their drafts outlive the turn that requested them.

use super::*;
use codex_protocol::items::AsyncUserInputQuestion;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn terminal_turns_preserve_question_identity_order_and_drafts() {
    for status in [
        AppServerTurnStatus::Completed,
        AppServerTurnStatus::Interrupted,
        AppServerTurnStatus::Failed,
    ] {
        for expanded in [false, true] {
            let (mut chat, _rx, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
            chat.thread_id = Some(ThreadId::new());
            handle_turn_started(&mut chat, "turn");
            chat.bottom_pane
                .set_composer_text("main draft".into(), Vec::new(), Vec::new());
            chat.add_async_questions(
                "questions",
                &[
                    AsyncUserInputQuestion {
                        title: "First?".into(),
                        options: None,
                    },
                    AsyncUserInputQuestion {
                        title: "Second?".into(),
                        options: None,
                    },
                ],
            );
            chat.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
            chat.bottom_pane.handle_paste("first draft".into());
            chat.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
            chat.bottom_pane.handle_paste("second draft".into());
            if !expanded {
                chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
                chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
            }
            let before = chat.bottom_pane.question_editor().capture();
            if status == AppServerTurnStatus::Failed {
                // A standalone error can precede the matching terminal notification.
                handle_error(&mut chat, "Turn failed", /*codex_error_info*/ None);
                assert_eq!(chat.bottom_pane.question_editor().capture(), before);
            }
            for _ in 0..2 {
                chat.handle_server_notification(
                    ServerNotification::TurnCompleted(TurnCompletedNotification {
                        thread_id: chat.thread_id.unwrap().to_string(),
                        turn: app_server_turn(
                            "turn",
                            status.clone(),
                            /*duration_ms*/ None,
                            (status == AppServerTurnStatus::Failed).then(|| AppServerTurnError {
                                misalignment: None,
                                message: "Turn failed".into(),
                                codex_error_info: None,
                                additional_details: None,
                            }),
                        ),
                    }),
                    /*replay_kind*/ None,
                );
                assert_eq!(chat.bottom_pane.question_editor().capture(), before);
                assert_eq!(chat.bottom_pane.composer_text(), "main draft");
                assert!(ops.try_recv().is_err());
            }
        }
    }
}
