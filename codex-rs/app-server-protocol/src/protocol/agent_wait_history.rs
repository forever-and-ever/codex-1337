use super::v2::CollabAgentTool;
use super::v2::CollabAgentToolCallStatus;
use super::v2::ThreadItemPayload;

/// Current-turn tracking also sees the legacy events emitted after each canonical item.
/// They cannot erase richer wait completion evidence or revive a terminal operation.
pub(super) fn retain_wait(existing: &ThreadItemPayload, incoming: &ThreadItemPayload) -> bool {
    match (existing, incoming) {
        (ThreadItemPayload::CodeModeOwned(old), ThreadItemPayload::CodeModeOwned(new)) => {
            old.owner == new.owner && retain_wait(old.payload(), new.payload())
        }
        (
            ThreadItemPayload::CollabAgentToolCall {
                id: old_id,
                tool: CollabAgentTool::Wait,
                status: old_status,
                wait_completion: old_completion,
                ..
            },
            ThreadItemPayload::CollabAgentToolCall {
                id: new_id,
                tool: CollabAgentTool::Wait,
                status: new_status,
                wait_completion: new_completion,
                ..
            },
        ) if old_id == new_id && *old_status != CollabAgentToolCallStatus::InProgress => {
            *new_status == CollabAgentToolCallStatus::InProgress
                || (old_completion.is_some() && new_completion.is_none())
        }
        _ => false,
    }
}

#[cfg(test)]
pub(crate) fn assert_wait_history_retains_completion(wait: &codex_protocol::items::TurnItem) {
    use super::thread_history::ThreadHistoryBuilder;
    use super::thread_history::build_turns_from_rollout_items;
    use codex_protocol::ThreadId;
    use codex_protocol::protocol::AgentStatus;
    use codex_protocol::protocol::CollabWaitingBeginEvent;
    use codex_protocol::protocol::CollabWaitingEndEvent;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::ItemCompletedEvent;
    use codex_protocol::protocol::TurnStartedEvent;
    use codex_rollout::RolloutItem;
    let thread_id = ThreadId::new();
    let receiver = ThreadId::new();
    let turn_id = "wait-history";
    let owner = crate::CodeModeOwner {
        thread_id,
        exec_call_id: "outer-exec".into(),
        cell_id: Some("cell".into()),
        started_at_ms: 0,
    };
    let begin = EventMsg::CollabWaitingBegin(CollabWaitingBeginEvent {
        call_id: wait.id(),
        started_at_ms: 0,
        sender_thread_id: thread_id,
        receiver_thread_ids: vec![receiver],
        receiver_agents: Vec::new(),
    });
    let end = EventMsg::CollabWaitingEnd(CollabWaitingEndEvent {
        call_id: wait.id(),
        completed_at_ms: 1_234,
        sender_thread_id: thread_id,
        statuses: [(
            receiver,
            AgentStatus::Errored("target failed, not the wait".into()),
        )]
        .into_iter()
        .collect(),
        agent_statuses: Vec::new(),
    });
    for owned in [false, true] {
        let wrap_event = |event| {
            if owned {
                EventMsg::CodeModeOwned(crate::CodeModeOwned::new(owner.clone(), Box::new(event)))
            } else {
                event
            }
        };
        let expected = ThreadItemPayload::from(wait.clone());
        let expected = if owned {
            ThreadItemPayload::CodeModeOwned(crate::CodeModeOwned::new(
                owner.clone(),
                Box::new(expected),
            ))
        } else {
            expected
        };
        let events = [
            EventMsg::TurnStarted(TurnStartedEvent {
                turn_attribution: None,
                turn_id: turn_id.into(),
                root_turn_id: None,
                trace_id: None,
                started_at: None,
                model_context_window: None,
                collaboration_mode_kind: codex_protocol::config_types::ModeKind::default(),
            }),
            wrap_event(begin.clone()),
            wrap_event(EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id,
                turn_id: turn_id.into(),
                item: wait.clone(),
                started_at_ms: Some(0),
                completed_at_ms: 1_234,
                content_token_count: None,
            })),
            wrap_event(end.clone()),
            wrap_event(begin.clone()),
        ];
        let mut history = ThreadHistoryBuilder::new();
        let mut rollout = Vec::new();
        for (index, event) in events.into_iter().enumerate() {
            history.handle_event(&event);
            rollout.push(RolloutItem::EventMsg(event));
            if index >= 2 {
                // Check every canonical/legacy prefix, not only a final canonical winner.
                assert_eq!(
                    history
                        .active_turn_snapshot()
                        .unwrap()
                        .item_payloads()
                        .cloned()
                        .collect::<Vec<_>>(),
                    vec![expected.clone()]
                );
                assert_eq!(
                    build_turns_from_rollout_items(&rollout)[0]
                        .item_payloads()
                        .cloned()
                        .collect::<Vec<_>>(),
                    vec![expected.clone()]
                );
            }
        }
    }
}
