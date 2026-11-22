//! Covers the model request sent by a real TUI side-chat fork after a completed parent turn.

#![cfg(test)]

use super::*;
use crate::app::session_lifecycle::ThreadAttachPresentation;
use crate::sidebar_chooser::side_chat_label;
use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use core_test_support::responses;
use pretty_assertions::assert_eq;

/// Captures one real model request and waits until its turn can be inherited by a fork.
pub(super) async fn ask(
    server: &mut AppServerSession,
    model: &wiremock::MockServer,
    thread: ThreadId,
    text: &str,
) -> Result<responses::ResponsesRequest> {
    let response_id = uuid::Uuid::new_v4().to_string();
    let item_id = format!("reply-{response_id}");
    let mock = responses::mount_sse_once(
        model,
        responses::sse(vec![
            responses::ev_assistant_message(&item_id, "Answer"),
            responses::ev_completed(&response_id),
        ]),
    )
    .await;
    let request_id = server.next_request_id();
    let _: TurnStartResponse = server
        .request_handle()
        .request_typed(ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.to_string(),
                input: vec![UserInput::Text {
                    text: text.to_owned(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;
    loop {
        let event = server.next_event().await.expect("connected app-server");
        if let AppServerEvent::ServerNotification(n) = event.event
            && matches!(n.as_ref(), ServerNotification::TurnCompleted(t) if t.thread_id == thread.to_string())
        {
            return Ok(mock.single_request());
        }
    }
}

#[tokio::test]
async fn temporary_side_chat_sends_policy_on_its_first_request() -> Result<()> {
    let mut app = make_test_app().await;
    let model = responses::start_mock_server().await;
    app_test_support::MockResponsesConfig::new(&model.uri()).write(&app.config.codex_home)?;
    app.config = app
        .rebuild_config_for_cwd(app.config.cwd.to_path_buf())
        .await?;
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let started = server.start_thread(&app.config).await?;
    let parent = started.session.thread_id;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        started,
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    ask(&mut server, &model, parent, "Parent question").await?;

    let prepared = server
        .prepare_side_thread(
            crate::app_server_session::session_preparation::RequestPolicy::Unbounded,
            &app.local_settings,
            app.side_fork_config(),
            parent,
            None,
        )
        .with_storage(crate::app_server_session::SideChatStorage::Temporary)
        .run()
        .await;
    server.apply_session_observations(prepared.observations)?;
    let fork = prepared.result?;
    let child = ask(&mut server, &model, fork.session.thread_id, "Side question").await?;
    let policy = child
        .message_input_texts("developer")
        .into_iter()
        .find(|text| {
            text.contains(
                &codex_context_fragments::SideConversationPolicy::Temporary.instructions(None),
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "first child request must contain the side-chat developer policy: {:?}",
                child.message_input_texts("developer")
            )
        });
    assert!(policy.contains("Scheduled updates"));
    assert!(policy.contains("main conversation without calling create_worktree"));
    assert!(
        child
            .message_input_texts("user")
            .iter()
            .any(|text| text.starts_with("Side conversation boundary."))
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_side_chat_keeps_its_name_and_uses_the_first_direct_input_for_its_label() -> Result<()>
{
    let question = "Why is CI failing on Windows?";
    let pixel = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
    let image = crate::chatwidget::UserMessage {
        remote_image_urls: vec![pixel.into()],
        .."".into()
    };
    for (message, label) in [(question.into(), question), (image, "[Image]")] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let model = responses::start_mock_server().await;
        app_test_support::MockResponsesConfig::new(&model.uri())
            .enable_feature(Feature::TuiWorkspace)
            .write(&app.config.codex_home)?;
        app.config = app
            .rebuild_config_for_cwd(app.config.cwd.to_path_buf())
            .await?;
        let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let started = server.start_thread(&app.config).await?;
        let parent = started.session.thread_id;
        ask(&mut server, &model, parent, "Prepare a release").await?;
        server
            .thread_set_name(parent, "Release plan".into())
            .await?;
        app.enqueue_primary_thread_session(started.session, started.turns)
            .await?;
        enable_side_retention(&mut app)?;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        tui.set_owned_screen(/*owned*/ true)?;
        let direct_turn = responses::mount_response_handler(&model, |_| {
            responses::sse_response(responses::sse(vec![
                responses::ev_assistant_message("reply", "Answer"),
                responses::ev_completed("done"),
            ]))
        })
        .await;
        Box::pin(app.handle_start_side(
            &mut tui,
            &mut server,
            parent,
            Some(message),
            crate::app_event::SideStartMode::New,
        ))
        .await?;

        time::timeout(Duration::from_secs(15), async {
            let mut child_finished = false;
            while !child_finished || !app.server_operations.is_empty() {
                tokio::select! {
                    Some(event) = events.recv() => {
                        assert!(!matches!(
                            &event,
                            AppEvent::ThreadTitleStarted { .. } | AppEvent::GeneratedThreadTitle { .. }
                        ));
                        boxed_future!(app.handle_event(&mut tui, &mut server, event)).await?;
                    }
                    Some(completion) = app.server_operations.control_rx.recv() => {
                        Box::pin(app.handle_server_completion(&mut tui, &mut server, completion)).await?;
                    }
                    Some(completion) = app.thread_delivery.completion_rx.recv() => {
                        app.handle_local_thread_completion(&mut server, completion).await?;
                    }
                    Some(event) = server.next_event() => {
                        if let AppServerEvent::ServerNotification(n) = &event.event
                            && let ServerNotification::TurnCompleted(turn) = n.as_ref()
                            && app.side_thread_for_parent(parent)
                                .is_some_and(|id| id.to_string() == turn.thread_id)
                        {
                            child_finished = true;
                        }
                        app.handle_backend_event(&server, event).await;
                    }
                }
            }
            Ok::<_, color_eyre::Report>(())
        })
        .await??;
        let saved = server
            .list_saved_side_chats(parent, app.config.codex_home.as_path())
            .await?;
        let [child] = saved.as_slice() else {
            panic!("expected one saved side chat")
        };
        assert_eq!(side_chat_label(child), Some(label));
        assert!(app.pending_thread_titles.is_empty());
        assert_eq!(direct_turn.requests().len(), 1, "no auxiliary title turn");
        let child_id = child.id;
        let fork = server
            .thread_read(child_id, /*include_turns*/ false)
            .await?;
        assert!(
            fork.name
                .as_deref()
                .is_some_and(|name| name.starts_with("Release plan_fork_")),
            "retained side chat keeps its allocated fork name"
        );

        app.rename_thread(&mut server, child_id, "Side follow-up".into(), Vec::new());
        Box::pin(app.settle_turn_submissions(&mut server)).await?;
        let saved = server
            .list_saved_side_chats(parent, app.config.codex_home.as_path())
            .await?;
        let [child] = saved.as_slice() else {
            panic!("expected one saved side chat")
        };
        assert_eq!(side_chat_label(child), Some("Side follow-up"));
        let parent = server.thread_read(parent, /*include_turns*/ false).await?;
        assert_eq!(parent.name.as_deref(), Some("Release plan"));
        assert_eq!(direct_turn.requests().len(), 1);
        server.shutdown().await?;
    }
    Ok(())
}
