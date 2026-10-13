use codex_protocol::ThreadId;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::security_risk::SecurityRiskScore;
use codex_protocol::user_input::UserInput;
use codex_rollout::CompactedItem;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutLine;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

use super::*;
use crate::ThreadHistoryBuilder;
use crate::protocol::v2::ThreadItem;
use crate::protocol::v2::TurnError;

#[test]
fn owned_capture_survives_live_history_and_independent_page_projection() {
    use crate::CodeModeOwned;
    use crate::CodeModeOwner;
    use crate::ThreadHistoryBuilder;
    use codex_protocol::items::FunctionCallOutputItem;
    use codex_protocol::models::FunctionCallOutputBody;

    // A suffix page has no preceding ownership hint; the capture must carry its owner.
    let thread_id = ThreadId::default();
    let owner = CodeModeOwner {
        thread_id,
        exec_call_id: "exec-1".into(),
        cell_id: Some("generation-1:cell-1".into()),
        started_at_ms: 1_000,
    };
    let native = match item_completed(
        thread_id,
        "turn-1",
        TurnItem::FunctionCallOutput(FunctionCallOutputItem {
            id: "output-1".into(),
            call_id: Some("exec-1".into()),
            name: "exec".into(),
            namespace: Some("functions".into()),
            output: FunctionCallOutputBody::Text("only what JS returned".into()),
        }),
    ) {
        RolloutItem::EventMsg(native) => native,
        _ => unreachable!(),
    };
    let event = EventMsg::CodeModeOwned(CodeModeOwned::new(owner.clone(), Box::new(native)));
    let projected = project(RolloutItem::EventMsg(event.clone()));
    let mut builder = ThreadHistoryBuilder::new();
    builder.handle_event(&EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: "turn-1".into(),
        turn_attribution: None,
        root_turn_id: None,
        trace_id: None,
        started_at: Some(1),
        model_context_window: None,
        collaboration_mode_kind: Default::default(),
    }));
    builder.handle_event(&event);
    builder.handle_event(&event);
    let history = builder.finish();
    assert_eq!(
        history[0].items,
        vec![projected.changed_items[0].item.clone()]
    );
    assert_eq!(history[0].items[0].id(), "output-1");
    let ThreadItem::CodeModeOwned(owned) = &history[0].items[0] else {
        panic!("capture lost its ownership");
    };
    assert_eq!(owned.owner, owner);
    let mut nested = serde_json::to_value(&event).unwrap();
    nested["payload"] = nested.clone();
    assert!(serde_json::from_value::<EventMsg>(nested).is_err());
}

#[test]
fn thought_completion_is_self_contained_and_response_scoped() {
    let event =
        EventMsg::RawResponseCompleted(codex_protocol::protocol::RawResponseCompletedEvent {
            response_id: "response-1".into(),
            token_usage: None,
            usage_metadata: Some(codex_protocol::ResponseUsageMetadata {
                reported_reasoning_tokens: Some(1300),
                ..Default::default()
            }),
            thought: Some(codex_protocol::ResponseThought {
                turn_id: "turn-1".into(),
                completed_at_ms: 12345,
                summary: vec!["First block".into(), "Second block".into()],
                content: vec!["Readable content".into()],
            }),
        });
    // An independent page needs no preceding reasoning item or turn-start event.
    let mut builder = ThreadHistoryBuilder::new();
    builder.handle_event(&event);
    builder.handle_event(&event);
    let mut next = event.clone();
    if let EventMsg::RawResponseCompleted(completed) = &mut next {
        completed.response_id = "response-2".into();
        completed.usage_metadata = None;
        let thought = completed.thought.as_mut().unwrap();
        thought.summary.clear();
        thought.content.clear();
    }
    builder.handle_event(&next);
    let turns = builder.finish();
    assert_eq!(
        turns[0].items,
        vec![
            ThreadItem::Thought {
                id: "thought:turn-1:response-1".into(),
                response_id: "response-1".into(),
                completed_at_ms: 12345,
                summary: vec!["First block".into(), "Second block".into()],
                content: vec!["Readable content".into()],
                reported_reasoning_tokens: Some(1300),
            },
            ThreadItem::Thought {
                id: "thought:turn-1:response-2".into(),
                response_id: "response-2".into(),
                completed_at_ms: 12345,
                summary: Vec::new(),
                content: Vec::new(),
                reported_reasoning_tokens: None,
            },
        ]
    );
    for (event, item) in [event, next.clone()].into_iter().zip(&turns[0].items) {
        assert_eq!(
            project(RolloutItem::EventMsg(event)),
            ThreadHistoryChangeSet {
                changed_items: vec![ThreadHistoryItemChange {
                    turn_id: "turn-1".into(),
                    item: item.clone(),
                    started_at_ms: None,
                    completed_at_ms: Some(12345),
                }],
                ..Default::default()
            }
        );
    }
    if let EventMsg::RawResponseCompleted(completed) = &mut next {
        completed.thought = None;
    }
    assert_eq!(
        project(RolloutItem::EventMsg(next)),
        ThreadHistoryChangeSet::default()
    );
}

#[test]
fn projects_turn_lifecycle_without_prior_builder_state() {
    let started = project(RolloutItem::EventMsg(EventMsg::TurnStarted(
        TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-1".to_string(),
            root_turn_id: Some("root-turn".into()),
            trace_id: None,
            started_at: Some(10),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        },
    )));
    let completed = project(RolloutItem::EventMsg(EventMsg::TurnComplete(
        TurnCompleteEvent {
            root_turn_id: Some("root-turn".into()),
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
            error: None,
            started_at: Some(10),
            completed_at: Some(20),
            duration_ms: Some(10_000),
            time_to_first_token_ms: None,
        },
    )));

    assert_eq!(started.changed_turns.len(), 1);
    assert_eq!(started.changed_turns[0].turn_id, "turn-1");
    assert_eq!(started.changed_turns[0].status, TurnStatus::InProgress);
    assert_eq!(started.changed_turns[0].started_at, Some(10));
    assert_eq!(
        started.changed_turns[0].root_turn_id.as_deref(),
        Some("root-turn")
    );
    assert_eq!(
        completed,
        ThreadHistoryChangeSet {
            changed_turns: vec![ThreadHistoryTurnMetadata {
                turn_id: "turn-1".to_string(),
                root_turn_id: Some("root-turn".into()),
                status: TurnStatus::Completed,
                error: None,
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(10_000),
            }],
            ..Default::default()
        }
    );
}

#[test]
fn projects_failed_turn_completion_as_snapshot() {
    let error = ErrorEvent {
        misalignment: None,
        message: "request failed".to_string(),
        codex_error_info: None,
    };

    let changes = project(RolloutItem::EventMsg(EventMsg::TurnComplete(
        TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
            error: Some(error),
            started_at: Some(10),
            completed_at: Some(20),
            duration_ms: Some(10_000),
            time_to_first_token_ms: None,
        },
    )));

    assert_eq!(
        changes,
        ThreadHistoryChangeSet {
            changed_turns: vec![ThreadHistoryTurnMetadata {
                turn_id: "turn-1".to_string(),
                root_turn_id: None,
                status: TurnStatus::Failed,
                error: Some(TurnError {
                    misalignment: None,
                    message: "request failed".to_string(),
                    codex_error_info: None,
                    additional_details: None,
                }),
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(10_000),
            }],
            ..Default::default()
        }
    );
}

#[test]
fn projects_completed_canonical_turn_items() {
    let thread_id = ThreadId::default();
    let user_item = TurnItem::UserMessage(UserMessageItem {
        source_response_item_id: None,
        id: "user-1".to_string(),
        client_id: None,
        content: vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }],
    });
    let agent_item = TurnItem::AgentMessage(AgentMessageItem {
        id: "agent-1".to_string(),
        content: vec![AgentMessageContent::Text {
            text: "done".to_string(),
        }],
        phase: None,
        memory_citation: None,
        delivery: None,
        questions: None,
    });

    let user_changes = project(item_completed(thread_id, "turn-1", user_item.clone()));
    let agent_changes = project(item_completed(thread_id, "turn-1", agent_item.clone()));

    assert_eq!(
        user_changes.changed_items,
        vec![ThreadHistoryItemChange {
            turn_id: "turn-1".to_string(),
            item: ThreadItem::from(user_item),
            started_at_ms: Some(100),
            completed_at_ms: Some(123),
        }]
    );
    assert_eq!(
        agent_changes.changed_items,
        vec![ThreadHistoryItemChange {
            turn_id: "turn-1".to_string(),
            item: ThreadItem::from(agent_item),
            started_at_ms: Some(100),
            completed_at_ms: Some(123),
        }]
    );
}

#[test]
fn projects_optional_completed_item_lifecycle_timestamps() {
    let item = TurnItem::UserMessage(UserMessageItem {
        source_response_item_id: None,
        id: "user-1".to_string(),
        client_id: None,
        content: Vec::new(),
    });

    for (started_at_ms, completed_at_ms, expected_completed_at_ms) in
        [(None, 123, Some(123)), (Some(100), 0, None)]
    {
        let changes = project(RolloutItem::EventMsg(EventMsg::ItemCompleted(
            ItemCompletedEvent {
                thread_id: ThreadId::default(),
                turn_id: "turn-1".to_string(),
                item: item.clone(),
                started_at_ms,
                completed_at_ms,
            },
        )));

        assert_eq!(
            changes,
            ThreadHistoryChangeSet {
                changed_items: vec![ThreadHistoryItemChange {
                    turn_id: "turn-1".to_string(),
                    item: ThreadItem::from(item.clone()),
                    started_at_ms,
                    completed_at_ms: expected_completed_at_ms,
                }],
                ..Default::default()
            }
        );
    }
}

#[test]
fn ignores_legacy_abort_without_turn_id_and_context_only_records() {
    let aborted = project(RolloutItem::EventMsg(EventMsg::TurnAborted(
        TurnAbortedEvent {
            root_turn_id: None,
            turn_id: None,
            reason: TurnAbortReason::Interrupted,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        },
    )));
    let compacted = project(RolloutItem::Compacted(CompactedItem {
        message: String::new(),
        replacement_history: None,
        retained_context: None,
        guardian_history: None,
        mcp_resource_origins: None,
        window_number: None,
        first_window_id: None,
        previous_window_id: None,
        window_id: None,
        compaction_response_id: None,
        latest_token_usage_record: None,
        resume_metadata: None,
    }));
    let security_risk = project(RolloutItem::SecurityRiskScore(SecurityRiskScore {
        scores: BTreeMap::from([("action_risk".to_string(), 0.92)]),
        call_id: None,
        action: None,
        sampled_at: None,
    }));

    assert!(aborted.is_empty());
    assert!(compacted.is_empty());
    assert!(security_risk.is_empty());
}

#[test]
fn projects_identified_turn_aborts() {
    let changes = project(RolloutItem::EventMsg(EventMsg::TurnAborted(
        TurnAbortedEvent {
            root_turn_id: Some("root-turn".into()),
            turn_id: Some("turn-1".to_string()),
            reason: TurnAbortReason::Interrupted,
            error: None,
            started_at: Some(10),
            completed_at: Some(20),
            duration_ms: Some(10_000),
        },
    )));

    assert_eq!(
        changes,
        ThreadHistoryChangeSet {
            changed_turns: vec![ThreadHistoryTurnMetadata {
                turn_id: "turn-1".to_string(),
                root_turn_id: Some("root-turn".into()),
                status: TurnStatus::Interrupted,
                error: None,
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(10_000),
            }],
            ..Default::default()
        }
    );
}

fn project(item: RolloutItem) -> ThreadHistoryChangeSet {
    project_rollout_line(&RolloutLine {
        timestamp: "2026-07-09T00:00:00.000Z".to_string(),
        ordinal: Some(7),
        item,
    })
}

fn item_completed(thread_id: ThreadId, turn_id: &str, item: TurnItem) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
        thread_id,
        turn_id: turn_id.to_string(),
        item,
        started_at_ms: Some(100),
        completed_at_ms: 123,
    }))
}
