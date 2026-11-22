//! Turn-scoped pairing for command starts synthesized before core launches a command.
//! A native terminal disarms its sentinel. Turn end drains the rest before `TurnCompleted`;
//! teardown drops them without I/O, leaving the client's owner reaping as the final boundary.

use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::thread_state::ThreadState;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItemPayload;
use codex_protocol::protocol::GuardianAssessmentEvent;
use codex_protocol::protocol::GuardianAssessmentStatus;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::Mutex;

#[derive(Default)]
pub(crate) struct CommandStarts(BTreeMap<String, Start>);

enum Start {
    Native,
    Synthesized(Box<CommandSentinel>),
}

enum Review {
    Pending,
    Approved,
}

struct CommandSentinel {
    notification: ItemStartedNotification,
    outgoing: ThreadScopedOutgoingMessageSender,
    review: Review,
}

impl CommandStarts {
    /// A canonical start proves approval and suppresses a second start notification.
    pub(crate) fn insert(&mut self, id: String) -> bool {
        match self.0.entry(id) {
            Entry::Vacant(slot) => {
                slot.insert(Start::Native);
                true
            }
            Entry::Occupied(mut slot) => {
                if let Start::Synthesized(sentinel) = slot.get_mut() {
                    sentinel.review = Review::Approved;
                }
                false
            }
        }
    }

    pub(crate) fn register(
        &mut self,
        notification: &ItemStartedNotification,
        outgoing: &ThreadScopedOutgoingMessageSender,
    ) -> bool {
        let Entry::Vacant(slot) = self.0.entry(notification.item.id().to_owned()) else {
            return false;
        };
        slot.insert(Start::Synthesized(Box::new(CommandSentinel {
            notification: notification.clone(),
            outgoing: outgoing.clone(),
            review: Review::Pending,
        })));
        true
    }

    pub(crate) fn remove(&mut self, id: &str) -> bool {
        self.0.remove(id).is_some()
    }

    fn approve(&mut self, id: &str) {
        if let Some(Start::Synthesized(sentinel)) = self.0.get_mut(id) {
            sentinel.review = Review::Approved;
        }
    }

    pub(crate) async fn drain(&mut self) {
        for start in std::mem::take(&mut self.0).into_values() {
            let Start::Synthesized(sentinel) = start else {
                continue;
            };
            let mut item = sentinel.notification.item;
            let ThreadItemPayload::CommandExecution { status, .. } = &mut item.payload else {
                unreachable!("only command starts acquire sentinels");
            };
            // Core reports interrupted executions as failed, not declined. Only a
            // command that never cleared review can be declined by turn teardown.
            *status = match sentinel.review {
                Review::Pending => CommandExecutionStatus::Declined,
                Review::Approved => CommandExecutionStatus::Failed,
            };
            sentinel
                .outgoing
                .send_server_notification(ServerNotification::ItemCompleted(
                    ItemCompletedNotification::new(
                        sentinel.notification.thread_id,
                        sentinel.notification.turn_id,
                        item.payload,
                        item.started_at_ms,
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
                            .unwrap_or_default(),
                    ),
                ))
                .await;
        }
    }
}

pub(crate) async fn note_review(
    turn_id: &str,
    assessment: &GuardianAssessmentEvent,
    thread_state: &Arc<Mutex<ThreadState>>,
) {
    if assessment.status == GuardianAssessmentStatus::Approved
        && let Some(id) = assessment.target_item_id.as_deref()
        && let Some(summary) = thread_state.lock().await.turn_summaries.get_mut(turn_id)
    {
        summary.command_execution_started.approve(id);
    }
}

pub(crate) async fn note_decision(
    thread_state: &Arc<Mutex<ThreadState>>,
    turn_id: &str,
    id: &str,
    completion_status: Option<&CommandExecutionStatus>,
) {
    if completion_status.is_none()
        && let Some(summary) = thread_state.lock().await.turn_summaries.get_mut(turn_id)
    {
        summary.command_execution_started.approve(id);
    }
}
