use super::*;
use crate::session::tests::make_session_and_context_with_rx;
use codex_protocol::items::TurnItem;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn early_terminal_and_late_capture_have_one_execution_lifetime() {
    let (session, turn, events) = Box::pin(make_session_and_context_with_rx()).await;
    let service = &session.services.code_mode_service;
    let start = service
        .transcript
        .open(
            Arc::downgrade(&session),
            Arc::clone(&turn),
            "exec-1".into(),
            "text('done')".into(),
            service.shutdown_token.clone(),
        )
        .await
        .unwrap();
    let scope = start.scope();
    let execution_hold = scope.execution_hold();
    assert!(matches!(
        session.runtime_holds.activity(),
        crate::runtime_holds::RuntimeActivity::Held { .. }
    ));
    let opened_owner = scope.owner();
    assert_eq!(opened_owner.cell_id, None);

    // Pure JS can finish inside execute(), before execute() returns its allocated ID.
    scope.finish(CodeModeStatus::Completed);
    scope.finish(CodeModeStatus::Failed);
    let cell = CellId::new("generation-1:cell-1".into());
    start.allocated(&cell).await;
    scope.terminal_admitted().await;
    assert!(matches!(
        session.runtime_holds.activity(),
        crate::runtime_holds::RuntimeActivity::Held { .. }
    ));
    drop(execution_hold);
    assert!(matches!(
        session.runtime_holds.activity(),
        crate::runtime_holds::RuntimeActivity::Quiet { .. }
    ));
    let owner = scope.owner();
    let actual = std::iter::from_fn(|| events.try_recv().ok())
        .map(|event| {
            let EventMsg::CodeModeOwned(owned) = event.msg else {
                panic!("execution fact must carry its owner");
            };
            let descriptor = owned.owner.clone();
            let (_, native) = owned.into_parts();
            let EventMsg::ItemCompleted(event) = *native else {
                panic!("expected fact")
            };
            let TurnItem::CodeMode(fact) = event.item else {
                panic!("expected fact")
            };
            (descriptor, fact.event)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (
                opened_owner,
                CodeModeEvent::Started {
                    call_id: "exec-1".into(),
                    script: Some("text('done')".into()),
                }
            ),
            (owner.clone(), CodeModeEvent::Allocated),
            (
                owner.clone(),
                CodeModeEvent::Finished {
                    status: CodeModeStatus::Completed
                }
            ),
        ]
    );
    assert!(service.transcript.state.lock().unwrap().active.is_empty());
    assert!(Arc::ptr_eq(
        &service.transcript.scope(&cell).unwrap(),
        &scope
    ));

    // A completed runtime may buffer its result for a later turn. Captures and token
    // measurements retain descriptors, not a running terminal task or alias map.
    scope.capture_alias("wait-1".into(), &turn);
    let captured_owner = service.transcript.owner("wait-1").unwrap();
    // Approval emits a synthetic command start before execution begins. It must carry
    // the same owner or the client creates an unrelated live command without its input.
    let approval = EventMsg::ExecApprovalRequest(
        serde_json::from_value(serde_json::json!({
            "call_id": "wait-1",
            "started_at_ms": 0,
            "command": ["true"],
            "cwd": "/workspace",
            "parsed_cmd": []
        }))
        .unwrap(),
    );
    let EventMsg::CodeModeOwned(approval) = service.transcript.wrap(approval, None) else {
        panic!("approval must carry its Code Mode owner");
    };
    assert_eq!(approval.owner, owner);
    assert!(matches!(
        approval.payload().as_ref(),
        EventMsg::ExecApprovalRequest(_)
    ));
    // Auto-review synthesizes the same command start but identifies the reviewed
    // invocation separately from the review itself.
    let assessment = EventMsg::GuardianAssessment(
        serde_json::from_value(serde_json::json!({
            "id": "review-wait-1",
            "target_item_id": "wait-1",
            "status": "in_progress",
            "action": {"type": "command", "source": "unified_exec", "command": "true", "cwd": "/workspace"}
        }))
        .unwrap(),
    );
    session.send_event(&turn, assessment).await;
    let assessment = events.recv().await.unwrap();
    assert_eq!(assessment.id, turn.sub_id);
    let EventMsg::CodeModeOwned(assessment) = assessment.msg else {
        panic!("auto-review must leave the emitting session with its Code Mode owner");
    };
    assert_eq!(assessment.owner, owner);
    assert!(
        matches!(assessment.payload().as_ref(), EventMsg::GuardianAssessment(event) if event.target_item_id.as_deref() == Some("wait-1"))
    );
    // Cancellation releases only its own wait; the tracker owns ordered completion.
    scope.capture_alias("wait-2".into(), &turn);
    let target = CodeModeOwner {
        exec_call_id: "other-exec".into(),
        ..owner.clone()
    };
    let returning = service
        .begin_process_wait("wait-1", target.clone(), Arc::clone(&turn))
        .await
        .unwrap();
    let cancelled = service
        .begin_process_wait("wait-2", target.clone(), Arc::clone(&turn))
        .await
        .unwrap();
    returning.finish().await;
    drop(cancelled);
    let wait_facts = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let mut facts = Vec::new();
        for _ in 0..4 {
            let event = events.recv().await.unwrap();
            let EventMsg::CodeModeOwned(owned) = event.msg else {
                panic!("wait fact requires its actor owner");
            };
            assert_eq!(owned.owner, owner);
            let (_, event) = owned.into_parts();
            let EventMsg::ItemCompleted(event) = *event else {
                panic!("expected wait fact");
            };
            let TurnItem::CodeMode(fact) = event.item else {
                panic!("expected Code Mode wait");
            };
            facts.push(fact.event);
        }
        facts
    })
    .await
    .unwrap();
    assert_eq!(
        wait_facts,
        vec![
            CodeModeEvent::WaitStarted {
                call_id: "wait-1".into(),
                target: Some(target.clone())
            },
            CodeModeEvent::WaitStarted {
                call_id: "wait-2".into(),
                target: Some(target.clone())
            },
            CodeModeEvent::WaitCompleted {
                call_id: "wait-1".into(),
                target: Some(target.clone())
            },
            CodeModeEvent::WaitCompleted {
                call_id: "wait-2".into(),
                target: Some(target)
            },
        ]
    );
    service
        .transcript
        .release_capture("wait-2", CaptureOrigin::Returned);
    service
        .transcript
        .release_capture("wait-1", CaptureOrigin::Returned);
    assert_eq!(service.transcript.owner("wait-1"), None);
    assert_eq!(captured_owner, owner);
    // Returned capture may beat deferred notify capture after terminal admission.
    // Neither order is allowed to steal the other accepted batch's descriptor.
    let receiving_queue = Arc::new(tokio::sync::Mutex::new(crate::state::TurnState::default()));
    service
        .transcript
        .notification(&owner, "exec-1".into(), Arc::downgrade(&receiving_queue))
        .accepted();
    service
        .transcript
        .notification(&owner, "exec-1".into(), Arc::downgrade(&receiving_queue))
        .accepted();
    service
        .transcript
        .release_capture("exec-1", CaptureOrigin::Returned);
    assert_eq!(service.transcript.owner("exec-1"), Some(owner.clone()));
    service
        .transcript
        .release_capture("exec-1", CaptureOrigin::Notification);
    assert_eq!(service.transcript.owner("exec-1"), Some(owner));
    service
        .transcript
        .release_capture("exec-1", CaptureOrigin::Notification);
    assert_eq!(service.transcript.owner("exec-1"), None);
    drop(scope);
    drop(turn);
    // Idle pruning is required: there need not be another execution to sweep a lease.
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let empty = {
                let state = service.transcript.state.lock().unwrap();
                state.aliases.is_empty() && state.cells.is_empty()
            };
            if empty {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    service.shutdown().await.unwrap();
    assert!(service.transcript.tasks.is_empty());
}

#[tokio::test]
async fn failed_allocation_closes_without_fabricating_a_runtime_cell() {
    let (session, turn, events) = Box::pin(make_session_and_context_with_rx()).await;
    let service = &session.services.code_mode_service;
    service
        .transcript
        .open(
            Arc::downgrade(&session),
            turn,
            "failed-exec".into(),
            "throw 1".into(),
            service.shutdown_token.clone(),
        )
        .await
        .unwrap()
        .failed()
        .await;
    let actual = std::iter::from_fn(|| events.try_recv().ok())
        .map(|event| {
            let EventMsg::CodeModeOwned(owned) = event.msg else {
                panic!("expected scope")
            };
            let (_, native) = owned.into_parts();
            let EventMsg::ItemCompleted(event) = *native else {
                panic!("expected fact")
            };
            let TurnItem::CodeMode(fact) = event.item else {
                panic!("expected fact")
            };
            (fact.cell_id, fact.event)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (
                None,
                CodeModeEvent::Started {
                    call_id: "failed-exec".into(),
                    script: Some("throw 1".into()),
                }
            ),
            (
                None,
                CodeModeEvent::Finished {
                    status: CodeModeStatus::Failed
                }
            ),
        ]
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "the witness deliberately blocks admission while teardown must cancel its drain"
)]
async fn service_teardown_cancels_blocked_transcript_admission() {
    let (session, turn, _) = Box::pin(make_session_and_context_with_rx()).await;
    let service = &session.services.code_mode_service;
    let starting = service
        .transcript
        .open(
            Arc::downgrade(&session),
            turn,
            "abandoned-exec".into(),
            "await new Promise(() => {})".into(),
            service.shutdown_token.clone(),
        )
        .await
        .unwrap();
    let scope = starting.scope();
    let wait = service
        .begin_process_wait("abandoned-exec", scope.owner(), Arc::clone(&scope.turn))
        .await
        .unwrap();
    let _blocked_admission = scope.admission.lock().await;
    // Admission/persistence/delivery all run inside the same cancellable drain.
    // Teardown must not join while waiting for an unavailable admission gate.
    drop(starting);
    let teardown = async {
        let (shutdown, ()) = tokio::join!(
            service.shutdown(),
            scope.record(
                &scope.turn,
                CodeModeEvent::ToolCall {
                    call_id: "blocked-child".into()
                }
            )
        );
        shutdown
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), teardown)
        .await
        .unwrap()
        .unwrap();
    wait.finish().await;
    assert!(service.transcript.tasks.is_empty());
}
