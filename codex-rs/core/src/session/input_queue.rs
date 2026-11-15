use crate::agent::api::AgentControl;
use crate::agent_communication::PENDING_MAILBOX_MESSAGES;
use crate::state::ActiveTurn;
use crate::state::MailboxDeliveryPhase;
use crate::state::TurnState;
use codex_diagnostics::GaugeGuard;
use codex_history::ResponseItemEnvelope;
use codex_protocol::ThreadId;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::NonSteerableTurnKind;
use codex_protocol::turn_input::NotSubmittedReason;
use codex_protocol::turn_input::PendingSteerMutationOutcome;
use codex_protocol::turn_input::TurnStartOptions;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::watch;

mod mailbox_batch;
mod task_admission;
use mailbox_batch::MailboxQueue;

mod interjection;
pub(crate) use interjection::Interjection;

/// Host capture metadata belonging to one input, including steers within another turn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputMetadata {
    /// Private extension data carried with this queued input through completion.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extension_data: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_order: Option<u64>,
    /// Optional host-assigned UUID for this particular Responses user item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_item_uuid: Option<uuid::Uuid>,
    #[serde(
        default,
        skip_serializing_if = "codex_history::UserInputOrigin::is_user"
    )]
    pub origin: codex_history::UserInputOrigin,
}

/// Input consumed by a regular turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TurnInput {
    UserInput {
        content: Vec<UserInput>,
        client_id: Option<String>,
        #[serde(flatten)]
        metadata: UserInputMetadata,
    },
    FunctionCallOutput(#[serde(with = "turn_input_response_item")] ResponseItemEnvelope),
    // Preserve the existing serialized format while carrying injection API metadata
    // through the in-memory queue.
    ResponseItem(#[serde(with = "turn_input_response_item")] ResponseItemEnvelope),
    InterAgentCommunication(crate::PendingAgentMessage),
    // History already owns this notification; only model consumption is pending.
    CodeModeNotification,
}

mod turn_input_response_item {
    use super::ResponseItem;
    use super::ResponseItemEnvelope;
    use serde::Deserialize;
    use serde::Deserializer;
    use serde::Serialize;
    use serde::Serializer;
    use serde::ser::Error as _;

    pub(super) fn serialize<S>(
        item: &ResponseItemEnvelope,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if item.metadata.is_some() {
            return Err(S::Error::custom(
                "annotated response items cannot cross the turn-input serialization boundary",
            ));
        }
        item.item.serialize(serializer)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<ResponseItemEnvelope, D::Error>
    where
        D: Deserializer<'de>,
    {
        ResponseItem::deserialize(deserializer).map(ResponseItemEnvelope::new)
    }
}

/// Steering takes precedence when several kinds of input are pending together.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum InputQueueActivity {
    Mailbox,
    Steer,
}

impl TurnInput {
    fn is_queue_only_mail(&self) -> bool {
        match self {
            Self::InterAgentCommunication(communication) => !communication.trigger_turn,
            Self::ResponseItem(ResponseItemEnvelope {
                item: ResponseItem::AgentMessage { .. },
                ..
            }) => true,
            Self::UserInput { .. }
            | Self::FunctionCallOutput(_)
            | Self::ResponseItem(_)
            | Self::CodeModeNotification => false,
        }
    }

    fn wait_activity(&self) -> InputQueueActivity {
        match self {
            Self::InterAgentCommunication(_)
            | Self::ResponseItem(ResponseItemEnvelope {
                item: ResponseItem::AgentMessage { .. },
                ..
            }) => InputQueueActivity::Mailbox,
            Self::UserInput { .. }
            | Self::FunctionCallOutput(_)
            | Self::ResponseItem(_)
            | Self::CodeModeNotification => InputQueueActivity::Steer,
        }
    }
}

/// Turn-local pending input storage owned by the input queue flow.
#[derive(Default)]
pub(crate) struct TurnInputQueue {
    items: Vec<TurnInput>,
    /// Activity from notifying writes. Passive response items do not wake a wait.
    notified_activity: Option<InputQueueActivity>,
}

/// Session-scoped pending input storage and active-turn mailbox delivery coordination.
pub(crate) struct InputQueue {
    activity_tx: watch::Sender<InputQueueActivity>,
    controller: Option<(ThreadId, Arc<dyn AgentControl>, watch::Receiver<bool>)>,
    mailbox_pending_mails: Mutex<MailboxQueue>,
    mailbox_batch_changed: tokio::sync::Notify,
}

pub(crate) struct PendingMailboxCommunication {
    pub(crate) communication: crate::PendingAgentMessage,
    start_options: TurnStartOptions,
    _diagnostics_guard: GaugeGuard,
}

impl InputQueue {
    pub(crate) fn new() -> Self {
        let (activity_tx, _) = watch::channel(InputQueueActivity::Mailbox);
        Self {
            activity_tx,
            controller: None,
            mailbox_pending_mails: Mutex::new(MailboxQueue::default()),
            mailbox_batch_changed: tokio::sync::Notify::new(),
        }
    }

    pub(crate) fn with_controller(thread_id: ThreadId, control: Arc<dyn AgentControl>) -> Self {
        let updates = control.watch_mailbox(thread_id);
        Self {
            controller: Some((thread_id, control, updates)),
            ..Self::new()
        }
    }

    pub(crate) fn mailbox_updates(&self) -> Option<watch::Receiver<bool>> {
        self.controller
            .as_ref()
            .map(|(_, _, updates)| updates.clone())
    }

    pub(crate) fn notify_mailbox(&self) {
        self.activity_tx.send_replace(InputQueueActivity::Mailbox);
    }

    fn read_mailbox(&self, pending: &mut VecDeque<PendingMailboxCommunication>) {
        if let Some((thread_id, control, _)) = &self.controller {
            pending.extend(
                control
                    .take_mailbox(*thread_id)
                    .into_iter()
                    .map(|communication| PendingMailboxCommunication {
                        communication,
                        start_options: TurnStartOptions::default(),
                        _diagnostics_guard: PENDING_MAILBOX_MESSAGES.track(),
                    }),
            );
        }
    }

    pub(crate) async fn subscribe_activity(
        &self,
        turn_state: Option<&Mutex<TurnState>>,
    ) -> (
        watch::Receiver<InputQueueActivity>,
        Option<InputQueueActivity>,
    ) {
        let activity_rx = self.activity_tx.subscribe();
        let turn_activity = if let Some(turn_state) = turn_state {
            turn_state.lock().await.pending_input.pending_activity()
        } else {
            None
        };
        let pending_activity = if let Some(activity) = turn_activity {
            Some(activity)
        } else if self.has_pending_mailbox_items().await {
            Some(InputQueueActivity::Mailbox)
        } else {
            None
        };
        (activity_rx, pending_activity)
    }

    pub(crate) async fn enqueue_mailbox_communication(
        &self,
        communication: crate::PendingAgentMessage,
        start_options: TurnStartOptions,
    ) {
        let mut queue = self.mailbox_pending_mails.lock().await;
        queue.release();
        let pending = &mut queue.ready;
        // Mail retained while unloaded precedes new submissions to the loaded session.
        self.read_mailbox(pending);
        pending.push_back(PendingMailboxCommunication {
            communication,
            start_options,
            _diagnostics_guard: PENDING_MAILBOX_MESSAGES.track(),
        });
        self.activity_tx.send_replace(InputQueueActivity::Mailbox);
    }

    pub(crate) async fn has_pending_mailbox_items(&self) -> bool {
        !self.mailbox_pending_mails.lock().await.ready.is_empty()
            || self
                .controller
                .as_ref()
                .is_some_and(|(_, _, updates)| *updates.borrow())
    }

    /// Native mailbox backlog qualifies regardless of turn ID or delivery phase.
    /// Injected mail qualifies only while its matching turn still accepts mail.
    pub(crate) async fn should_preempt_sampling_for_mail(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
        sub_id: &str,
    ) -> bool {
        if self.has_pending_mailbox_items().await {
            return true;
        }
        let Some(turn_state) = self.turn_state_for_sub_id(active_turn, sub_id).await else {
            return false;
        };
        let turn_state = turn_state.lock().await;
        turn_state.accepts_mailbox_delivery_for_current_turn()
            && turn_state.pending_input.items.iter().any(|input| {
                matches!(
                    input,
                    TurnInput::ResponseItem(ResponseItemEnvelope {
                        item: ResponseItem::AgentMessage { .. },
                        ..
                    })
                )
            })
    }

    pub(crate) async fn has_trigger_turn_mailbox_items(&self) -> bool {
        self.mailbox_pending_mails
            .lock()
            .await
            .ready
            .iter()
            .any(|mail| mail.communication.trigger_turn)
    }

    pub(crate) async fn drain_mailbox(&self) -> Vec<PendingMailboxCommunication> {
        let mut pending = self.mailbox_pending_mails.lock().await;
        self.read_mailbox(&mut pending.ready);
        pending.ready.drain(..).collect()
    }

    pub(crate) async fn drain_mailbox_input_items(&self) -> (Vec<TurnInput>, TurnStartOptions) {
        let pending_mails = self.drain_mailbox().await;
        let start_options = task_admission::start_options(pending_mails.iter());
        let items = pending_mails
            .into_iter()
            .map(|mail| TurnInput::InterAgentCommunication(mail.communication))
            .collect();
        (items, start_options)
    }

    pub(crate) async fn turn_state_for_sub_id(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
        sub_id: &str,
    ) -> Option<Arc<Mutex<TurnState>>> {
        let active = active_turn.lock().await;
        active.as_ref().and_then(|active_turn| {
            active_turn
                .task
                .as_ref()
                .is_some_and(|task| task.turn_context.sub_id == sub_id)
                .then(|| Arc::clone(&active_turn.turn_state))
        })
    }

    /// Clear any pending waiters and input buffered for the current turn.
    pub(crate) async fn clear_pending(&self, active_turn: &ActiveTurn) {
        let mut turn_state = active_turn.turn_state.lock().await;
        turn_state.clear_pending_waiters();
        // Pending mail can own extension attachments whose destructors need the lock released.
        let pending_input = std::mem::take(&mut turn_state.pending_input);
        drop(turn_state);
        drop(pending_input);
    }

    pub(crate) async fn replace_pending_steer(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
        client_id: &str,
        content: Vec<UserInput>,
    ) -> PendingSteerMutationOutcome {
        self.mutate_pending_steer(active_turn, |pending_input| {
            let Some(TurnInput::UserInput {
                content: pending_content,
                ..
            }) = pending_input.items.iter_mut().find(|input| {
                matches!(
                    input,
                    TurnInput::UserInput {
                        client_id: Some(pending_client_id),
                        ..
                    } if pending_client_id == client_id
                )
            })
            else {
                return false;
            };
            *pending_content = content;
            true
        })
        .await
    }

    pub(crate) async fn cancel_pending_steer(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
        client_id: &str,
    ) -> PendingSteerMutationOutcome {
        self.mutate_pending_steer(active_turn, |pending_input| {
            let Some(index) = pending_input.items.iter().position(|input| {
                matches!(
                    input,
                    TurnInput::UserInput {
                        client_id: Some(pending_client_id),
                        ..
                    } if pending_client_id == client_id
                )
            }) else {
                return false;
            };
            pending_input.items.remove(index);
            true
        })
        .await
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    async fn mutate_pending_steer(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
        mutate: impl FnOnce(&mut TurnInputQueue) -> bool,
    ) -> PendingSteerMutationOutcome {
        let mut active = active_turn.lock().await;
        let Some(active_turn) = active.as_mut() else {
            return PendingSteerMutationOutcome::NotSubmitted {
                reason: NotSubmittedReason::NoActiveTurn,
            };
        };
        let Some(active_task) = active_turn.task.as_ref() else {
            return PendingSteerMutationOutcome::NotSubmitted {
                reason: NotSubmittedReason::NoActiveTurn,
            };
        };
        let turn_kind = match active_task.kind {
            crate::state::TaskKind::Regular => None,
            crate::state::TaskKind::Review => Some(NonSteerableTurnKind::Review),
            crate::state::TaskKind::Compact => Some(NonSteerableTurnKind::Compact),
        };
        if let Some(turn_kind) = turn_kind {
            return PendingSteerMutationOutcome::NotSubmitted {
                reason: NotSubmittedReason::ActiveTurnNotSteerable { turn_kind },
            };
        }

        let mut turn_state = active_turn.turn_state.lock().await;
        if mutate(&mut turn_state.pending_input) {
            PendingSteerMutationOutcome::Applied
        } else {
            PendingSteerMutationOutcome::NotFound
        }
    }

    pub(crate) async fn defer_mailbox_delivery_to_next_turn(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
        sub_id: &str,
    ) {
        let turn_state = self.turn_state_for_sub_id(active_turn, sub_id).await;
        let Some(turn_state) = turn_state else {
            return;
        };
        let mut turn_state = turn_state.lock().await;
        // Explicit same-turn work still needs a follow-up. Queue-only agent mail does not: keep
        // it pending so task completion records it for the next turn without sampling again.
        if turn_state
            .pending_input
            .items
            .iter()
            .any(|input| !input.is_queue_only_mail())
        {
            return;
        }
        turn_state.set_mailbox_delivery_phase(MailboxDeliveryPhase::NextTurn);
    }

    pub(crate) async fn accept_mailbox_delivery_for_current_turn(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
        sub_id: &str,
    ) {
        let turn_state = self.turn_state_for_sub_id(active_turn, sub_id).await;
        let Some(turn_state) = turn_state else {
            return;
        };
        self.accept_mailbox_delivery_for_turn_state(turn_state.as_ref())
            .await;
    }

    pub(super) async fn accept_mailbox_delivery_for_turn_state(
        &self,
        turn_state: &Mutex<TurnState>,
    ) {
        turn_state
            .lock()
            .await
            .accept_mailbox_delivery_for_current_turn();
    }

    pub(super) async fn extend_pending_input_and_notify_for_turn_state(
        &self,
        turn_state: &Mutex<TurnState>,
        input: Vec<TurnInput>,
    ) {
        let activity = input.iter().map(TurnInput::wait_activity).max();
        let queue_only_mail = !input.is_empty() && input.iter().all(TurnInput::is_queue_only_mail);
        {
            let mut turn_state = turn_state.lock().await;
            turn_state.pending_input.notified_activity =
                turn_state.pending_input.notified_activity.max(activity);
            turn_state.pending_input.items.extend(input);
            if !queue_only_mail {
                turn_state.accept_mailbox_delivery_for_current_turn();
            }
        }
        self.activity_tx
            .send_replace(activity.unwrap_or(InputQueueActivity::Steer));
    }

    pub(crate) async fn extend_pending_input_for_turn_state(
        &self,
        turn_state: &Mutex<TurnState>,
        input: Vec<TurnInput>,
    ) {
        turn_state.lock().await.pending_input.items.extend(input);
    }

    pub(crate) async fn take_pending_input_for_turn_state(
        &self,
        turn_state: &Mutex<TurnState>,
    ) -> Vec<TurnInput> {
        std::mem::take(&mut turn_state.lock().await.pending_input).items
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn get_pending_input(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
    ) -> (Vec<TurnInput>, TurnStartOptions) {
        let (pending_input, accepts_mailbox_delivery) = {
            let mut active = active_turn.lock().await;
            match active.as_mut() {
                Some(active_turn) => {
                    let mut turn_state = active_turn.turn_state.lock().await;
                    let accepts_mailbox_delivery =
                        turn_state.accepts_mailbox_delivery_for_current_turn();
                    let pending_input = if accepts_mailbox_delivery {
                        std::mem::take(&mut turn_state.pending_input).items
                    } else {
                        Vec::new()
                    };
                    (pending_input, accepts_mailbox_delivery)
                }
                None => (Vec::new(), true),
            }
        };
        if !accepts_mailbox_delivery {
            return (pending_input, TurnStartOptions::default());
        }
        let (mailbox_items, start_options) = self.drain_mailbox_input_items().await;
        if pending_input.is_empty() {
            (mailbox_items, start_options)
        } else {
            let mut pending_input = pending_input;
            pending_input.extend(mailbox_items);
            (pending_input, start_options)
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state reads must remain atomic"
    )]
    pub(crate) async fn has_pending_input(&self, active_turn: &Mutex<Option<ActiveTurn>>) -> bool {
        let (has_turn_pending_input, accepts_mailbox_delivery) = {
            let active = active_turn.lock().await;
            match active.as_ref() {
                Some(active_turn) => {
                    let turn_state = active_turn.turn_state.lock().await;
                    (
                        !turn_state.pending_input.is_empty(),
                        turn_state.accepts_mailbox_delivery_for_current_turn(),
                    )
                }
                None => (false, true),
            }
        };
        if !accepts_mailbox_delivery {
            return false;
        }
        if has_turn_pending_input {
            return true;
        }
        self.has_pending_mailbox_items().await
    }
}

impl TurnInputQueue {
    fn has_user_input(&self) -> bool {
        self.items
            .iter()
            .any(|input| matches!(input, TurnInput::UserInput { .. }))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn pending_activity(&self) -> Option<InputQueueActivity> {
        if self.notified_activity == Some(InputQueueActivity::Steer) {
            return self.notified_activity;
        }
        self.items
            .iter()
            .filter(|input| !matches!(input, TurnInput::ResponseItem(_)))
            .map(TurnInput::wait_activity)
            .chain(self.notified_activity)
            .max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_history::CodexHarnessMetadata;
    use codex_protocol::AgentPath;
    use codex_protocol::protocol::InterAgentCommunication;
    use codex_protocol::user_input::UserInput;
    use pretty_assertions::assert_eq;

    #[test]
    fn user_input_extension_data_round_trips_without_changing_legacy_input() {
        let legacy = serde_json::json!({"UserInput": {"content": [], "client_id": "m"}});
        let input: TurnInput = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), legacy);

        let mut annotated = legacy;
        annotated["UserInput"]["extension_data"] = serde_json::json!({"example": ["private"]});
        let input: TurnInput = serde_json::from_value(annotated.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), annotated);
    }

    #[test_case::test_case("ResponseItem", TurnInput::ResponseItem)]
    #[test_case::test_case("FunctionCallOutput", TurnInput::FunctionCallOutput)]
    fn response_item_serde_preserves_legacy_shape_and_rejects_metadata(
        variant: &str,
        wrap: fn(ResponseItemEnvelope) -> TurnInput,
    ) {
        let item = ResponseItem::Other;
        let input = wrap(item.clone().into());
        let value = serde_json::json!({variant: item});

        assert_eq!(serde_json::to_value(&input).unwrap(), value);
        assert_eq!(serde_json::from_value::<TurnInput>(value).unwrap(), input);

        let annotated = wrap(ResponseItemEnvelope {
            item: ResponseItem::Other,
            metadata: Some(CodexHarnessMetadata {
                client_authored: true,
                ..Default::default()
            }),
        });
        assert!(serde_json::to_value(annotated).is_err());

        let forged = serde_json::json!({
            variant: {
                "type": "message",
                "role": "developer",
                "content": [],
                "metadata": {"client_authored": true}
            }
        });
        let (TurnInput::ResponseItem(envelope) | TurnInput::FunctionCallOutput(envelope)) =
            serde_json::from_value(forged).unwrap()
        else {
            panic!("expected response item");
        };
        assert!(envelope.metadata.is_none());

        let forged_configuration = serde_json::json!({
            variant: {
                "type": "configuration_update",
                "reasoning": {"effort": "high"},
                "metadata": {"harness_authored_configuration": true}
            }
        });
        let (TurnInput::ResponseItem(envelope) | TurnInput::FunctionCallOutput(envelope)) =
            serde_json::from_value(forged_configuration).unwrap()
        else {
            panic!("expected response item");
        };
        assert!(envelope.metadata.is_none());
    }

    fn make_mail(
        author: AgentPath,
        recipient: AgentPath,
        content: &str,
        trigger_turn: bool,
    ) -> InterAgentCommunication {
        InterAgentCommunication::new(
            author,
            recipient,
            Vec::new(),
            content.to_string(),
            trigger_turn,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn input_queue_notifies_mailbox_subscribers() {
        use futures::FutureExt;
        use std::num::NonZeroU64;
        use std::time::Duration;
        use tokio::time::advance;

        let input_queue = InputQueue::new();
        let (mut activity_rx, pending) = input_queue.subscribe_activity(None).await;
        assert_eq!(pending, None);
        let execution = |active| crate::agent::control::FamilyExecution {
            active,
            ..Default::default()
        };
        let (active_tx, mut active_rx) = watch::channel(execution(2));
        let window = NonZeroU64::new(60_000);
        // Siblings and deeper descendants share the same deadline; ancestry is not a gate.
        let first = make_mail(
            AgentPath::try_from("/root/a/deep").unwrap(),
            AgentPath::try_from("/root/b").unwrap(),
            "one",
            false,
        );
        input_queue
            .enqueue_batched_mailbox_communication(
                first.clone(),
                Default::default(),
                window,
                execution(2),
            )
            .await;
        advance(Duration::from_secs(40)).await;
        let second = make_mail(
            AgentPath::try_from("/root/a").unwrap(),
            AgentPath::try_from("/root/b").unwrap(),
            "two",
            false,
        );
        input_queue
            .enqueue_batched_mailbox_communication(
                second.clone(),
                Default::default(),
                window,
                execution(2),
            )
            .await;
        advance(Duration::from_secs(19)).await;
        assert!(
            input_queue
                .wait_for_mailbox_release(&mut active_rx)
                .now_or_never()
                .is_none()
        );
        assert!(!activity_rx.has_changed().unwrap());
        assert!(!input_queue.has_pending_mailbox_items().await);
        // Another arrival must not restart the first message's fixed window.
        advance(Duration::from_secs(1)).await;
        input_queue.wait_for_mailbox_release(&mut active_rx).await;
        activity_rx.changed().await.unwrap();
        assert_eq!(
            *activity_rx.borrow_and_update(),
            InputQueueActivity::Mailbox
        );
        assert_eq!(
            input_queue.drain_mailbox_input_items().await.0,
            vec![
                TurnInput::InterAgentCommunication(first.clone().into()),
                TurnInput::InterAgentCommunication(second.into())
            ]
        );
        assert!(input_queue.drain_mailbox_input_items().await.0.is_empty());

        input_queue
            .enqueue_batched_mailbox_communication(
                first.clone(),
                Default::default(),
                window,
                execution(2),
            )
            .await;
        // watch may coalesce a drop and restart; eligibility must remain irrevocable.
        active_tx.send_modify(|state| {
            state.active = 1;
            state.release_generation += 1;
        });
        active_tx.send_modify(|state| state.active = 2);
        input_queue.wait_for_mailbox_release(&mut active_rx).await;
        assert_eq!(
            input_queue.drain_mailbox_input_items().await.0,
            vec![TurnInput::InterAgentCommunication(first.clone().into())]
        );
        // Root instructions and explicit new tasks bypass the hold without overtaking older mail.
        for (author, task) in [
            (AgentPath::root(), false),
            (AgentPath::try_from("/root/a").unwrap(), true),
        ] {
            input_queue
                .enqueue_batched_mailbox_communication(
                    first.clone(),
                    Default::default(),
                    window,
                    execution(2),
                )
                .await;
            let instruction = make_mail(
                author,
                AgentPath::try_from("/root/b").unwrap(),
                "continue",
                task,
            );
            input_queue
                .enqueue_batched_mailbox_communication(
                    instruction.clone(),
                    Default::default(),
                    window,
                    execution(2),
                )
                .await;
            assert_eq!(
                input_queue.drain_mailbox_input_items().await.0,
                vec![
                    TurnInput::InterAgentCommunication(first.clone().into()),
                    TurnInput::InterAgentCommunication(instruction.into())
                ]
            );
        }
        for active in [0, 1] {
            input_queue
                .enqueue_batched_mailbox_communication(
                    first.clone(),
                    Default::default(),
                    window,
                    execution(active),
                )
                .await;
            assert!(input_queue.has_pending_mailbox_items().await);
            assert!(!input_queue.has_held_mailbox_items().await);
            input_queue.drain_mailbox().await;
        }
    }

    #[tokio::test]
    async fn input_queue_distinguishes_new_agent_input_from_pending_steering() {
        let input_queue = InputQueue::new();
        let turn_state = Mutex::new(TurnState::default());
        let (mut activity_rx, pending_activity) =
            input_queue.subscribe_activity(Some(&turn_state)).await;
        assert_eq!(pending_activity, None);

        input_queue
            .extend_pending_input_and_notify_for_turn_state(
                &turn_state,
                vec![TurnInput::UserInput {
                    metadata: Default::default(),
                    content: vec![UserInput::Text {
                        text: "steer".to_string(),
                        text_elements: Vec::new(),
                    }],
                    client_id: None,
                }],
            )
            .await;

        activity_rx.changed().await.expect("steer update");
        assert_eq!(*activity_rx.borrow_and_update(), InputQueueActivity::Steer);

        let mail = make_mail(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "follow-up",
            /*trigger_turn*/ true,
        );
        input_queue
            .extend_pending_input_and_notify_for_turn_state(
                &turn_state,
                vec![TurnInput::InterAgentCommunication(mail.into())],
            )
            .await;
        activity_rx.changed().await.expect("mailbox update");
        assert_eq!(
            *activity_rx.borrow_and_update(),
            InputQueueActivity::Mailbox
        );
        assert_eq!(
            input_queue.subscribe_activity(Some(&turn_state)).await.1,
            Some(InputQueueActivity::Steer)
        );
    }

    #[tokio::test]
    async fn input_queue_reports_already_pending_steer() {
        let input_queue = InputQueue::new();
        let turn_state = Mutex::new(TurnState::default());
        let passive_output: TurnInput = serde_json::from_value(serde_json::json!({
            "ResponseItem": {"type": "function_call_output", "name": "notify", "output": "passive"}
        }))
        .unwrap();
        input_queue
            .extend_pending_input_for_turn_state(&turn_state, vec![passive_output.clone()])
            .await;
        assert_eq!(
            input_queue.subscribe_activity(Some(&turn_state)).await.1,
            None
        );
        input_queue
            .extend_pending_input_and_notify_for_turn_state(
                &turn_state,
                vec![passive_output.clone()],
            )
            .await;
        assert_eq!(
            input_queue.subscribe_activity(Some(&turn_state)).await.1,
            Some(InputQueueActivity::Steer)
        );
        // Consuming the injection must not make later passive context wake a wait.
        input_queue
            .take_pending_input_for_turn_state(&turn_state)
            .await;
        input_queue
            .extend_pending_input_for_turn_state(&turn_state, vec![passive_output])
            .await;
        assert_eq!(
            input_queue.subscribe_activity(Some(&turn_state)).await.1,
            None
        );
        let communication = make_mail(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "already pending mail",
            /*trigger_turn*/ false,
        );
        input_queue
            .extend_pending_input_for_turn_state(
                &turn_state,
                vec![TurnInput::InterAgentCommunication(communication.into())],
            )
            .await;
        assert_eq!(
            input_queue.subscribe_activity(Some(&turn_state)).await.1,
            Some(InputQueueActivity::Mailbox)
        );
        input_queue
            .extend_pending_input_and_notify_for_turn_state(
                &turn_state,
                vec![TurnInput::UserInput {
                    metadata: Default::default(),
                    content: vec![UserInput::Text {
                        text: "already pending".to_string(),
                        text_elements: Vec::new(),
                    }],
                    client_id: None,
                }],
            )
            .await;

        let (_activity_rx, pending_activity) =
            input_queue.subscribe_activity(Some(&turn_state)).await;

        assert_eq!(pending_activity, Some(InputQueueActivity::Steer));
    }

    #[tokio::test]
    async fn input_queue_drains_mailbox_in_delivery_order() {
        let input_queue = InputQueue::new();
        let mail_one = make_mail(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "one",
            /*trigger_turn*/ false,
        );
        let mail_two = make_mail(
            AgentPath::try_from("/root/worker").expect("agent path"),
            AgentPath::root(),
            "two",
            /*trigger_turn*/ true,
        );

        input_queue
            .enqueue_mailbox_communication(mail_one.clone().into(), Default::default())
            .await;
        input_queue
            .enqueue_mailbox_communication(mail_two.clone().into(), Default::default())
            .await;

        assert_eq!(
            input_queue.drain_mailbox_input_items().await.0,
            vec![
                TurnInput::InterAgentCommunication(mail_one.into()),
                TurnInput::InterAgentCommunication(mail_two.into())
            ]
        );
        assert!(!input_queue.has_pending_mailbox_items().await);
    }

    #[tokio::test]
    async fn input_queue_uses_unambiguous_trigger_parent_and_first_root() {
        let (parent, peer, root, root2) = (Some("a"), Some("b"), Some("r"), Some("s"));
        for (pending_mails, expected_parent_turn_id, expected_root_turn_id) in [
            (Vec::new(), None, None),
            (vec![(false, Some("q"), root)], None, None),
            (vec![(true, Some(""), root)], None, None),
            (vec![(true, Some("   "), root)], None, None),
            (vec![(true, None, root)], None, None),
            (vec![(true, parent, None)], parent, None),
            (vec![(true, parent, Some(""))], parent, None),
            (vec![(true, parent, root), (true, peer, root)], None, root),
            (vec![(true, parent, root), (true, peer, root2)], None, root),
            (vec![(true, parent, root), (true, None, root)], None, root),
            (
                vec![(true, parent, root), (true, parent, root)],
                parent,
                root,
            ),
            (
                vec![(false, Some("q"), root2), (true, parent, root)],
                parent,
                root,
            ),
        ] {
            let input_queue = InputQueue::new();
            for (trigger_turn, parent_turn_id, root_turn_id) in pending_mails {
                input_queue
                    .enqueue_mailbox_communication(
                        make_mail(AgentPath::root(), AgentPath::root(), "task", trigger_turn)
                            .into(),
                        TurnStartOptions {
                            parent_turn_id: parent_turn_id.map(str::to_string),
                            root_turn_id: root_turn_id.map(str::to_string),
                            ..Default::default()
                        },
                    )
                    .await;
            }
            let (_, start_options) = input_queue.drain_mailbox_input_items().await;
            assert_eq!(
                start_options.parent_turn_id.as_deref(),
                expected_parent_turn_id
            );
            assert_eq!(start_options.root_turn_id.as_deref(), expected_root_turn_id);
        }
    }

    #[tokio::test]
    async fn input_queue_uses_latest_followup_choice_and_ignores_queue_only_mail() {
        use codex_protocol::turn_input::CyberAccessProgram;

        for latest in [Some(CyberAccessProgram::Standard), None] {
            let input_queue = InputQueue::new();
            for (trigger_turn, program) in [
                (true, Some(CyberAccessProgram::DaybreakBlue)),
                (true, latest),
                (false, Some(CyberAccessProgram::DaybreakRed)),
            ] {
                input_queue
                    .enqueue_mailbox_communication(
                        make_mail(AgentPath::root(), AgentPath::root(), "task", trigger_turn)
                            .into(),
                        TurnStartOptions {
                            cyber_access_program: program,
                            ..Default::default()
                        },
                    )
                    .await;
            }
            let (_, start_options) = input_queue.drain_mailbox_input_items().await;
            assert_eq!(start_options.cyber_access_program, latest);
        }
    }

    #[tokio::test]
    async fn input_queue_tracks_pending_trigger_turn_mail() {
        let input_queue = InputQueue::new();

        let queued_mail = make_mail(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "queued",
            /*trigger_turn*/ false,
        );
        input_queue
            .enqueue_mailbox_communication(queued_mail.into(), Default::default())
            .await;
        assert!(!input_queue.has_trigger_turn_mailbox_items().await);

        let trigger_mail = make_mail(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "wake",
            /*trigger_turn*/ true,
        );
        input_queue
            .enqueue_mailbox_communication(trigger_mail.into(), Default::default())
            .await;
        assert!(input_queue.has_trigger_turn_mailbox_items().await);
    }
}
