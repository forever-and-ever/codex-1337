use super::*;
use crate::bottom_pane::MentionBinding;
use codex_app_server_protocol::ThreadFarmMode;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn immediate_farm_preserves_rich_draft_and_rejects_unrepresentable_remote_images() {
    for (command, mode) in [
        (SlashCommand::Farm, ThreadFarmMode::FullHistory),
        (SlashCommand::FarmFresh, ThreadFarmMode::Fresh),
    ] {
        let (mut chat, mut events, mut ops) =
            Box::pin(make_chatwidget_manual(/*model_override*/ None)).await;
        chat.thread_id = Some(ThreadId::new());
        chat.on_task_started();
        let text = format!("/{} inspect $audit", command.command());
        chat.bottom_pane.set_composer_text_with_mention_bindings(
            text.clone(),
            Vec::new(),
            Vec::new(),
            vec![MentionBinding {
                sigil: '$',
                mention: "audit".into(),
                path: "skill:///tmp/audit/SKILL.md".into(),
            }],
            crate::bottom_pane::ShellPrefixHandling::Literal,
        );
        chat.set_remote_image_urls(vec!["data:image/png;base64,inline".into()]);
        let before = chat.bottom_pane.composer_draft_snapshot();
        // The queue binding must delegate immediately rather than append a parent follow-up.
        chat.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let submission = loop {
            if let AppEvent::Farm {
                mode: actual,
                submission,
                ..
            } = events.try_recv().expect("farm event")
            {
                assert_eq!(actual, mode);
                break submission;
            }
        };
        let mut after = chat.bottom_pane.composer_draft_snapshot();
        // Submission records activity/recall, not a change to the structured draft.
        after.startup_local_history = before.startup_local_history.clone();
        after.last_composer_activity_at = before.last_composer_activity_at;
        assert_eq!(after, before);
        assert_eq!(
            submission.input,
            vec![
                UserInput::Image {
                    image: codex_app_server_protocol::ImageReference::Inline {
                        url: "data:image/png;base64,inline".into()
                    },
                    detail: None
                },
                UserInput::Text {
                    text: "inspect $audit".into(),
                    text_elements: vec![codex_app_server_protocol::TextElement::new(
                        codex_app_server_protocol::ByteRange { start: 8, end: 14 },
                        Some("$audit".into()),
                    )]
                },
                UserInput::Skill {
                    name: "audit".into(),
                    path: PathBuf::from("/tmp/audit/SKILL.md")
                },
            ]
        );
        let mut after = chat.bottom_pane.composer_draft_snapshot();
        after.startup_local_history = before.startup_local_history.clone();
        after.last_composer_activity_at = before.last_composer_activity_at;
        assert_eq!(after, before);
        assert_no_submit_op(&mut ops);

        if command == SlashCommand::Farm {
            chat.open_farm_recovery("retained-request".into());
            assert_chatwidget_snapshot!(
                "farm_receipt_recovery",
                render_bottom_popup(&chat, /*width*/ 80)
            );
            chat.handle_key_event(KeyCode::Enter.into());
            let mut reconciled = false;
            while let Ok(event) = events.try_recv() {
                if let AppEvent::ReconcileFarm { request_id } = event {
                    assert_eq!(request_id, "retained-request");
                    reconciled = true;
                }
            }
            assert!(reconciled);
            chat.open_farm_recovery("retained-request".into());
            chat.handle_key_event(KeyCode::Down.into());
            chat.handle_key_event(KeyCode::Enter.into());
            let mut discarded = false;
            while let Ok(event) = events.try_recv() {
                if let AppEvent::DiscardFarm { request_id } = event {
                    assert_eq!(request_id, "retained-request");
                    discarded = true;
                }
            }
            assert!(discarded);
            assert_no_submit_op(&mut ops);
        }

        chat.bottom_pane.set_composer_text(
            text,
            Vec::new(),
            vec![PathBuf::from("/tmp/farm-image.png")],
        );
        chat.snapshot_local_images = true;
        let before = chat.bottom_pane.composer_draft_snapshot();
        chat.handle_key_event(KeyCode::Enter.into());
        let mut after = chat.bottom_pane.composer_draft_snapshot();
        after.startup_local_history = before.startup_local_history.clone();
        after.last_composer_activity_at = before.last_composer_activity_at;
        assert_eq!(after, before);
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(event, AppEvent::Farm { .. }));
        }
        assert_no_submit_op(&mut ops);
    }
}
