//! Decode v1/v2 reload input once; only canonical queue state leaves this boundary.

use super::super::ThreadComposerState;
use super::super::ThreadInputState;
use super::super::submission_dispatch::UserTurnDispatch;
use super::InputQueueState;
use super::MessageDelivery;
use super::PendingSteer;
use super::PendingTurnStart;
use super::QueuedUserMessage;
use super::SteerInput;
use super::SteerInterrupt;
use super::SubmittedUserInput;
use super::UserMessage;
use super::UserMessageHistoryRecord;
use super::UserMessageSource;
use super::UserTurnInput;
use super::VecDeque;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Deserialize;
use serde::de::Error;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyQueuedMessage {
    user_message: UserMessage,
    action: crate::bottom_pane::QueuedInputAction,
    #[serde(default)]
    delivery: MessageDelivery,
    pending_pastes: Vec<(String, String)>,
    source: UserMessageSource,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Decode-only historical wire layout must retain its independent legacy flags"
)]
struct LegacyInput {
    questions: Option<crate::bottom_pane::QuestionState>,
    pending_thread_settings: Option<codex_app_server_protocol::ThreadSettingsUpdatedNotification>,
    composer: Option<ThreadComposerState>,
    safety_buffering_prompt: Option<UserMessage>,
    safety_buffering_source: UserMessageSource,
    pending_steer: Option<PendingSteer>,
    rejected_steers_queue: VecDeque<UserMessage>,
    rejected_steer_sources: VecDeque<UserMessageSource>,
    rejected_steer_history_records: VecDeque<UserMessageHistoryRecord>,
    queued_user_messages: VecDeque<LegacyQueuedMessage>,
    queued_user_message_history_records: VecDeque<UserMessageHistoryRecord>,
    recovered_queue: bool,
    #[serde(default)]
    reconnect_pending: bool,
    user_turn_pending_start: bool,
    submit_pending_steer_after_interrupt: bool,
    #[serde(default)]
    pending_user_message_client_id: Option<String>,
    current_collaboration_mode: CollaborationMode,
    active_collaboration_mask: Option<CollaborationModeMask>,
    plan_mode_reasoning_effort: Option<ReasoningEffort>,
    task_running: bool,
    agent_turn_running: bool,
}

impl ThreadInputState {
    pub(crate) fn decode_retained(value: serde_json::Value) -> serde_json::Result<Self> {
        if value.get("input_queue").is_some() {
            serde_json::from_value(value)
        } else {
            serde_json::from_value::<LegacyInput>(value)?.into_current()
        }
    }
}

impl LegacyInput {
    fn into_current(self) -> serde_json::Result<ThreadInputState> {
        // Missing records had explicit defaults in the old reducer. Extra records
        // have no message owner: rejecting preserves the supervisor's checkpoint
        // instead of guessing which history or source should be discarded.
        if self.queued_user_message_history_records.len() > self.queued_user_messages.len()
            || self.rejected_steer_history_records.len() > self.rejected_steers_queue.len()
            || self.rejected_steer_sources.len() > self.rejected_steers_queue.len()
        {
            return Err(serde_json::Error::custom("orphaned legacy input records"));
        }
        let mut history = self.queued_user_message_history_records.into_iter();
        let queued_user_messages = self
            .queued_user_messages
            .into_iter()
            .map(|message| QueuedUserMessage {
                user_message: message.user_message,
                prepared_images: None,
                action: message.action,
                delivery: message.delivery,
                pending_pastes: message.pending_pastes,
                source: message.source,
                dispatch: UserTurnDispatch::default(),
                history_record: history
                    .next()
                    .unwrap_or(UserMessageHistoryRecord::UserMessageText),
            })
            .collect();
        let mut history = self.rejected_steer_history_records.into_iter();
        let mut sources = self.rejected_steer_sources.into_iter();
        let rejected_steers_queue = self
            .rejected_steers_queue
            .into_iter()
            .map(|message| QueuedUserMessage {
                source: sources.next().unwrap_or(UserMessageSource::Prompt),
                history_record: history
                    .next()
                    .unwrap_or(UserMessageHistoryRecord::UserMessageText),
                ..QueuedUserMessage::from(message)
            })
            .collect();
        let turn = match (
            self.user_turn_pending_start,
            self.safety_buffering_prompt,
            self.pending_user_message_client_id,
        ) {
            (pending, Some(message), Some(client_id)) => {
                let input = SubmittedUserInput {
                    client_id,
                    message,
                    history: UserMessageHistoryRecord::UserMessageText,
                    source: self.safety_buffering_source,
                    dispatch: UserTurnDispatch::default(),
                };
                if pending {
                    UserTurnInput::AwaitingStart(PendingTurnStart::Submitted(input))
                } else {
                    UserTurnInput::Started(input)
                }
            }
            (true, Some(message), None) => {
                UserTurnInput::AwaitingStart(PendingTurnStart::PreparedRetry {
                    message,
                    source: self.safety_buffering_source,
                })
            }
            (false, Some(message), None) => UserTurnInput::RetainedRetry {
                message,
                source: self.safety_buffering_source,
            },
            // Old finalization retired the prompt without clearing its receipt ID;
            // compact/review/shell could then mark that same state pending again.
            // Unavailable recovery also left the ID after moving the whole prompt
            // into an exact-ID Unconfirmed queue entry. Without a prompt this ID
            // cannot enrich a submission or reconcile receipt: normalize the stale
            // artifact, preserving any actionable identity in the queue itself.
            (true, None, _) => UserTurnInput::AwaitingStart(PendingTurnStart::Operation),
            (false, None, _) => UserTurnInput::Idle,
        };
        let pending_steer =
            self.pending_steer
                .map_or(SteerInput::Idle, |steer| SteerInput::Pending {
                    steer: Box::new(steer),
                    interrupt: if self.submit_pending_steer_after_interrupt {
                        SteerInterrupt::SubmitAsTurn
                    } else {
                        SteerInterrupt::RestoreDraft
                    },
                });
        Ok(ThreadInputState {
            provider_error: None,
            questions: self.questions,
            pending_thread_settings: self.pending_thread_settings,
            initial_user_message: None,
            status_surface_overrides: super::super::StatusSurfaceOverrides::default(),
            composer: self.composer,
            input_queue: InputQueueState {
                queued_user_messages,
                rejected_steers_queue,
                turn,
                pending_steer,
                recovered_queue: self.recovered_queue,
                rate_limit_recovery_pending: false,
            },
            reconnect_pending: self.reconnect_pending,
            current_collaboration_mode: self.current_collaboration_mode,
            active_collaboration_mask: self.active_collaboration_mask,
            plan_mode_reasoning_effort: self.plan_mode_reasoning_effort,
            task_running: self.task_running,
            agent_turn_running: self.agent_turn_running,
        })
    }
}
