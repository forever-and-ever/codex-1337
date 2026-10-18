use super::*;
use crate::app::thread_delivery::LocalThreadCompletion;
use pretty_assertions::assert_eq;

// Approval payloads must outlive transcript retention: evicting one strands a server that is still waiting for the answer.
#[test]
fn pending_approval_survives_transcript_eviction() {
    let mut store = ThreadEventStore::new(/*capacity*/ 1);
    let request = exec_approval_request(
        ThreadId::new(),
        "turn-1",
        "call-1",
        /*approval_id*/ None,
    );
    let history = HistoryLookupResponse::Entry {
        offset: 0,
        log_id: 1,
        entry: None,
    };
    store.push_request(request.clone());
    store.push_buffered_event(ThreadBufferedEvent::HistoryEntryResponse(history.clone()));
    assert_eq!(store.pending_replay_requests(), vec![request.clone()]);
    let events = store.snapshot().events;
    assert_eq!(events.len(), 2);
    assert!(
        matches!(&events[0], ThreadBufferedEvent::HistoryEntryResponse(actual) if actual == &history)
    );
    assert!(
        matches!(&events[1], ThreadBufferedEvent::Request(actual) if actual.as_ref() == &request)
    );
}

#[tokio::test]
async fn bounded_thread_admission_preserves_fifo_with_pending_history_effects() -> Result<()> {
    // The former detached Full sender was not yet reserving a slot when a newer direct send
    // could take it. Keep the receiver locally taken, and interleave widget/history effects.
    let (mut app, mut effects, _operations) = Box::pin(make_test_app_with_channels()).await;
    let thread_id = ThreadId::new();
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
    app.activate_thread_channel(thread_id).await;
    app.chat_widget.handle_thread_session(test_thread_session(
        thread_id,
        test_path_buf("/tmp/project"),
    ));
    while effects.try_recv().is_ok() {}
    let earlier =
        ServerNotification::ItemCompleted(codex_app_server_protocol::ItemCompletedNotification {
            thread_id: thread_id.to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: 0,
            item: ThreadItem::UserMessage {
                source_response_item_id: None,
                id: "earlier-user".to_string(),
                client_id: None,
                content: vec![AppServerUserInput::Text {
                    text: "earlier transcript effect".to_string(),
                    text_elements: Vec::new(),
                }],
            },
        });
    let request = exec_approval_request(thread_id, "turn-1", "call-1", /*approval_id*/ None);
    app.enqueue_thread_notification(thread_id, earlier).await?;
    app.enqueue_thread_request(thread_id, request.clone())
        .await?;
    assert!(app.thread_delivery.is_blocked());

    let history = HistoryLookupResponse::Entry {
        offset: 0,
        log_id: 1,
        entry: Some("later lookup".to_string()),
    };
    app.thread_delivery
        .completion_tx
        .send(LocalThreadCompletion::HistoryLookup {
            thread_id,
            event: history.clone(),
        })
        .await?;
    let mut receiver = app.active_thread_rx.take().expect("active receiver");
    app.handle_thread_event_now_recovering_file_changes(receiver.try_recv()?)
        .await;
    assert!(
        !effects.is_empty(),
        "the older notification generated history effects"
    );
    assert_eq!(app.thread_delivery.completion_rx.len(), 1);

    let (mut server, _requests, proxy) =
        Box::pin(session_lifecycle_requests::start_recording_app_server(
            &app.config,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
        ))
        .await?;
    let mut tui = tui::test_support::make_test_tui()?;
    while let Ok(effect) = effects.try_recv() {
        Box::pin(app.handle_event(&mut tui, &mut server, effect)).await?;
    }
    assert!(app.transcript_cells.iter().any(|cell| {
        lines_to_single_string(&cell.display_lines(/*width*/ 80))
            .contains("earlier transcript effect")
    }));
    app.retry_pending_thread_delivery().await;
    assert!(!app.thread_delivery.is_blocked());
    assert_eq!(
        app.thread_event_channels[&thread_id]
            .store
            .lock()
            .await
            .pending_replay_requests(),
        vec![request.clone()]
    );
    let newer = app.thread_delivery.completion_rx.try_recv()?;
    app.handle_local_thread_completion(&mut server, newer)
        .await?;
    assert!(app.thread_delivery.is_blocked());
    let ThreadBufferedEvent::Request(older) = receiver.try_recv()? else {
        panic!("older approval must precede newer lookup")
    };
    assert_eq!(*older, request);
    app.retry_pending_thread_delivery().await;
    let ThreadBufferedEvent::HistoryEntryResponse(actual) = receiver.try_recv()? else {
        panic!("newer lookup follows the approval")
    };
    assert_eq!(actual, history);
    assert!(!app.thread_delivery.is_blocked());
    app.active_thread_rx = Some(receiver);
    server.shutdown().await?;
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn pending_approval_detach_uses_existing_replay_without_a_dormant_send() -> Result<()> {
    let mut app = Box::pin(make_test_app()).await;
    let thread_id = ThreadId::new();
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
    app.activate_thread_channel(thread_id).await;
    let request = exec_approval_request(thread_id, "turn-1", "call-1", /*approval_id*/ None);
    app.enqueue_thread_notification(thread_id, turn_started_notification(thread_id, "turn-1"))
        .await?;
    app.enqueue_thread_request(thread_id, request.clone())
        .await?;
    assert!(app.thread_delivery.is_blocked());
    app.store_active_thread_receiver().await;
    app.active_thread_id = None;
    app.retry_pending_thread_delivery().await;
    assert!(!app.thread_delivery.is_blocked());
    assert_eq!(
        app.thread_event_channels[&thread_id]
            .store
            .lock()
            .await
            .pending_replay_requests(),
        vec![request]
    );
    Ok(())
}

#[tokio::test]
async fn initial_replay_is_incremental_and_pending_batch_survives_detach_replacement() -> Result<()>
{
    let (mut app, mut effects, _operations) = Box::pin(make_test_app_with_channels()).await;
    let thread_id = ThreadId::new();
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
    set_test_initial_prompt(&mut app, "held startup draft".to_string());
    let request = exec_approval_request(thread_id, "turn-1", "call-1", /*approval_id*/ None);
    let batch = HistoryLookupResponse::Batch {
        cursor: codex_message_history::HistoryBatchCursor::new(/*end_offset*/ 1),
        log_id: 1,
        entries: vec![HistoryBatchEntryResponse {
            offset: 0,
            entry: Some("retained batch".to_string()),
        }],
        next_older_cursor: None,
    };
    app.pending_primary_events.extend([
        ThreadBufferedEvent::Notification(Box::new(turn_started_notification(thread_id, "turn-1"))),
        ThreadBufferedEvent::Request(Box::new(request.clone())),
        ThreadBufferedEvent::HistoryEntryResponse(batch.clone()),
    ]);
    app.enqueue_primary_thread_session(
        test_thread_session(thread_id, test_path_buf("/tmp/project")),
        Vec::new(),
    )
    .await?;
    assert_eq!(app.pending_primary_events.len(), 2);
    app.admit_next_primary_event().await?;
    assert!(app.thread_delivery.is_blocked());
    assert_eq!(app.pending_primary_events.len(), 1);
    assert!(
        !std::iter::from_fn(|| effects.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::SubmitThreadOp {
                op: AppCommand::UserTurn { .. },
                ..
            } | AppEvent::CodexOp(AppCommand::UserTurn { .. })
        ))
    );

    let mut receiver = app.active_thread_rx.take().expect("active receiver");
    receiver.try_recv()?;
    app.retry_pending_thread_delivery().await;
    let ThreadBufferedEvent::Request(actual) = receiver.try_recv()? else {
        panic!("buffered approval")
    };
    assert_eq!(*actual, request);
    // Fill the receiver before the last initial batch; popping it must not release the draft.
    app.enqueue_thread_notification(
        thread_id,
        token_usage_notification(thread_id, "turn-1", Some(100)),
    )
    .await?;
    app.admit_next_primary_event().await?;
    assert!(app.pending_primary_events.is_empty());
    assert!(app.thread_delivery.is_blocked());
    assert_eq!(app.thread_delivery.primary_replay_thread, Some(thread_id));
    assert!(
        !std::iter::from_fn(|| effects.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::SubmitThreadOp {
                op: AppCommand::UserTurn { .. },
                ..
            } | AppEvent::CodexOp(AppCommand::UserTurn { .. })
        ))
    );

    app.active_thread_rx = Some(receiver);
    app.store_active_thread_receiver().await;
    app.active_thread_id = None;
    // Reconnect replaces sender/receiver and moves the store, retaining only notifications.
    let channel = app
        .thread_event_channels
        .get_mut(&thread_id)
        .expect("thread channel");
    let replacement = ThreadEventChannel::new(/*capacity*/ 1);
    let mut store = std::mem::replace(
        &mut *channel.store.lock().await,
        ThreadEventStore::new(/*capacity*/ 1),
    );
    store
        .buffer
        .retain(|event| matches!(event, ThreadBufferedEvent::Notification(_)));
    *replacement.store.lock().await = store;
    *channel = replacement;
    app.retry_pending_thread_delivery().await;
    assert!(!app.thread_delivery.is_blocked());
    let snapshot = app.thread_event_channels[&thread_id]
        .store
        .lock()
        .await
        .snapshot();
    assert!(snapshot.events.iter().any(|event| matches!(event, ThreadBufferedEvent::HistoryEntryResponse(actual) if *actual == batch)));
    assert_eq!(app.chat_widget.composer_text_with_pending(), "");

    // A reconnected channel can already be active when retry runs. Its fresh approval ledger
    // must not receive a request left pending against the replaced sender/store.
    app.activate_thread_channel(thread_id).await;
    app.enqueue_thread_notification(thread_id, turn_started_notification(thread_id, "turn-2"))
        .await?;
    app.enqueue_thread_request(thread_id, request).await?;
    assert!(app.thread_delivery.is_blocked());
    let replacement = ThreadEventChannel::new(/*capacity*/ 1);
    replacement.store.lock().await.active = true;
    app.thread_event_channels.insert(thread_id, replacement);
    app.retry_pending_thread_delivery().await;
    assert!(!app.thread_delivery.is_blocked());
    assert!(
        app.thread_event_channels
            .get_mut(&thread_id)
            .expect("replacement channel")
            .receiver
            .as_mut()
            .expect("replacement receiver")
            .try_recv()
            .is_err()
    );
    assert!(
        app.thread_event_channels[&thread_id]
            .store
            .lock()
            .await
            .pending_replay_requests()
            .is_empty()
    );
    Ok(())
}
