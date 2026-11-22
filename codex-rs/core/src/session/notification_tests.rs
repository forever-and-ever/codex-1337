//! Notification admission preserves continuation, receiving ownership, and output budgets.

use super::*;
use codex_user_input::ResolvedUserInput;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_finish_preserves_notification_budget_and_queued_user_input() {
    let (session, turn, events) = make_session_and_context_with_rx().await;
    session
        .spawn_task(
            Arc::clone(&turn),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: false,
            },
        )
        .await
        .unwrap();
    let text = "notification diagnostic line\n".repeat(500);
    let notification = |text| ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: "call-a".to_string(),
        name: Some("exec".to_string()),
        output: FunctionCallOutputPayload::from_text(text),
        internal_chat_message_metadata_passthrough: None,
    };
    let owner = codex_protocol::code_mode::CodeModeOwner {
        thread_id: session.thread_id,
        exec_call_id: "call-a".into(),
        cell_id: None,
        started_at_ms: 1,
    };
    // This is the final-answer boundary: mailbox delivery is deferred and the
    // next-step decision depends on has_pending_input, not just a watch wakeup.
    session
        .input_queue
        .defer_mailbox_delivery_to_next_turn(&session.active_turn, &turn.sub_id)
        .await;
    assert!(
        !session
            .input_queue
            .has_pending_input(&session.active_turn)
            .await
    );
    let (mut activity, _) = session.input_queue.subscribe_activity(None).await;
    session
        .inject_code_mode_notification(
            owner.clone(),
            ResponseItemEnvelope {
                item: notification(text.clone()),
                metadata: Some(CodexHarnessMetadata {
                    history_truncation_token_limit: Some(120),
                    ..Default::default()
                }),
            },
        )
        .await
        .unwrap();
    assert!(activity.has_changed().unwrap());
    assert_eq!(
        *activity.borrow_and_update(),
        input_queue::InputQueueActivity::Steer
    );
    let admitted_history = session.clone_history().await;
    assert_eq!(raw_history_items(&admitted_history).len(), 1);
    assert!(
        session
            .input_queue
            .has_pending_input(&session.active_turn)
            .await
    );

    let turn_state = Arc::clone(
        &session
            .active_turn
            .lock()
            .await
            .as_ref()
            .unwrap()
            .turn_state,
    );
    assert_eq!(
        session
            .input_queue
            .subscribe_activity(Some(&turn_state))
            .await
            .1,
        Some(input_queue::InputQueueActivity::Steer),
    );

    // Terminal cleanup detaches Task before draining input. A notification in
    // that interval must still use the retained receiving queue and descriptor.
    let task = session
        .active_turn
        .lock()
        .await
        .as_mut()
        .unwrap()
        .task
        .take();
    let late_text = "notification during terminal cleanup".to_string();
    session
        .inject_code_mode_notification(owner.clone(), notification(late_text.clone()).into())
        .await
        .unwrap();
    assert_eq!(raw_history_items(&session.clone_history().await).len(), 1);
    session.active_turn.lock().await.as_mut().unwrap().task = task;

    let user_input = vec![ResolvedUserInput::text(
        "keep this queued user input".to_string(),
    )];
    assert!(matches!(
        submit_steer_only(&session, user_input.clone(), &turn.sub_id).await,
        TurnInputSubmission::Steered { .. }
    ));
    let mut current = turn.initial_settings.as_ref().clone();
    Arc::make_mut(&mut current.model_info).truncation_policy =
        codex_protocol::openai_models::TruncationPolicyConfig::tokens(/*limit*/ 400);
    turn.next_step_settings.store(Arc::new(current));

    // Stall recording the first batch, then inject while finalization is in progress.
    let state = session.state.lock().await;
    let finish = session.on_task_finished(Arc::clone(&turn), /*task_result*/ Ok(None));
    let late = vec![notification("late notification".to_string())];
    tokio::join!(finish, async {
        while session
            .active_turn
            .lock()
            .await
            .as_ref()
            .expect("turn remains active while final recording is blocked")
            .finishing
            .is_none()
        {
            tokio::task::yield_now().await;
        }
        // Admission is closed even if recording has not yet reached the history lock.
        let notice = vec![notification("producer still running".to_string())];
        assert_eq!(session.inject_if_running(notice.clone()).await, Err(notice));
        let inject = session.inject_client_response_items(late.clone(), &turn);
        tokio::pin!(inject);
        assert!(futures::poll!(inject.as_mut()).is_pending());
        drop(state);
        inject.await;
    });

    let expected = vec![
        notification(codex_utils_output_truncation::truncate_text(
            &text,
            codex_utils_output_truncation::TruncationPolicy::Tokens(120),
        )),
        notification(late_text),
        session.response_item_from_user_input(&user_input),
        late[0].clone(),
    ];
    let history = session.clone_history().await;
    // on_task_finished detaches Task before recording pending input. Attribution
    // must follow its retained TurnState queue, not the old Task/worker context.
    let captured = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| {
            let EventMsg::CodeModeOwned(owned) = event.msg else {
                return None;
            };
            let (descriptor, native) = owned.into_parts();
            let EventMsg::ItemCompleted(event) = *native else {
                return None;
            };
            let codex_protocol::items::TurnItem::FunctionCallOutput(output) = event.item else {
                return None;
            };
            Some((descriptor, output.output))
        })
        .collect::<Vec<_>>();
    let ResponseItem::CustomToolCallOutput { output, .. } = &expected[0] else {
        unreachable!()
    };
    let ResponseItem::CustomToolCallOutput {
        output: late_output,
        ..
    } = &expected[1]
    else {
        unreachable!()
    };
    assert_eq!(
        captured,
        vec![
            (owner.clone(), output.body.clone()),
            (owner, late_output.body.clone())
        ]
    );
    assert_eq!(
        strip_response_item_ids(&strip_metadata_from_items(&raw_history_items(&history))),
        strip_response_item_ids(&strip_metadata_from_items(&expected)),
    );
}
