//! Conversation-owned queued input and its submission transitions.
//!
//! Active and parked threads carry this same value. Editor resources, startup
//! handoff and immutable server projections belong to the live frontend.

use std::borrow::Cow;
use std::collections::VecDeque;

use super::MessageDelivery;
use super::PendingSteer;
use super::PendingSteerReplacement;
use super::QueuedUserMessage;
use super::UserMessage;
use super::UserMessageHistoryRecord;
use super::UserMessageSource;
use super::user_message_preview_text;

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct PendingInputPreview {
    pub(super) queued_messages: Vec<String>,
    pub(super) pending_steer: Option<String>,
    pub(super) rejected_steers: Vec<String>,
}

#[derive(Debug, Default)]
pub(super) struct InputQueueFrontend {
    pub(super) startup_submission: Option<crate::bottom_pane::ComposerDraftSnapshot>,
    pub(super) server_messages: Vec<codex_app_server_protocol::QueuedSubmission>,
    pub(super) server_previews: Vec<String>,
    pub(super) suppress_queue_autosend: bool,
    pub(super) transcript_copy: std::sync::Weak<crate::copy_input_guard::CopyInputGuard>,
    pub(super) automatic_model_switch_pending: bool,
}

impl InputQueueFrontend {
    pub(super) fn submissions_paused(&self) -> bool {
        self.suppress_queue_autosend || self.transcript_copy.strong_count() > 0
    }

    pub(super) fn clear(&mut self) {
        self.startup_submission = None;
        self.server_messages.clear();
        self.server_previews.clear();
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SubmittedUserInput {
    pub(super) client_id: String,
    pub(super) message: UserMessage,
    pub(super) history: UserMessageHistoryRecord,
    pub(super) source: UserMessageSource,
    #[serde(
        default,
        skip_serializing_if = "super::submission_dispatch::UserTurnDispatch::is_frontend"
    )]
    pub(super) dispatch: super::submission_dispatch::UserTurnDispatch,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) enum PendingTurnStart {
    Operation,
    PreparedRetry {
        message: UserMessage,
        source: UserMessageSource,
    },
    Submitted(SubmittedUserInput),
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) enum UserTurnInput {
    #[default]
    Idle,
    AwaitingStart(PendingTurnStart),
    Started(SubmittedUserInput),
    RetainedRetry {
        message: UserMessage,
        source: UserMessageSource,
    },
}

impl UserTurnInput {
    pub(super) fn is_pending(&self) -> bool {
        matches!(self, Self::AwaitingStart(_))
    }

    pub(super) fn submitted(&self) -> Option<&SubmittedUserInput> {
        match self {
            Self::AwaitingStart(PendingTurnStart::Submitted(input)) | Self::Started(input) => {
                Some(input)
            }
            _ => None,
        }
    }

    pub(super) fn prompt(&self) -> Option<&UserMessage> {
        match self {
            Self::AwaitingStart(PendingTurnStart::PreparedRetry { message, .. })
            | Self::RetainedRetry { message, .. } => Some(message),
            _ => self.submitted().map(|input| &input.message),
        }
    }

    pub(super) fn started(&mut self) {
        *self = match std::mem::take(self) {
            Self::AwaitingStart(PendingTurnStart::Submitted(input)) | Self::Started(input) => {
                Self::Started(input)
            }
            Self::AwaitingStart(PendingTurnStart::PreparedRetry { message, source })
            | Self::RetainedRetry { message, source } => Self::RetainedRetry { message, source },
            _ => Self::Idle,
        };
    }

    pub(super) fn into_queued(self, uncertain: bool) -> Option<QueuedUserMessage> {
        match self {
            Self::AwaitingStart(PendingTurnStart::Submitted(input)) => Some(QueuedUserMessage {
                delivery: if uncertain {
                    MessageDelivery::Unconfirmed(Some(input.client_id))
                } else {
                    MessageDelivery::Unsent
                },
                source: input.source,
                dispatch: input.dispatch,
                history_record: input.history,
                ..QueuedUserMessage::from(input.message)
            }),
            Self::AwaitingStart(PendingTurnStart::PreparedRetry { message, source }) => {
                Some(QueuedUserMessage {
                    delivery: if uncertain {
                        MessageDelivery::Unconfirmed(None)
                    } else {
                        MessageDelivery::Unsent
                    },
                    source,
                    ..QueuedUserMessage::from(message)
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) enum SteerInterrupt {
    #[default]
    RestoreDraft,
    SubmitAsTurn,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) enum SteerInput {
    #[default]
    Idle,
    Pending {
        steer: Box<PendingSteer>,
        interrupt: SteerInterrupt,
    },
    Replacing {
        replacement: Box<PendingSteerReplacement>,
        interrupt: SteerInterrupt,
    },
}

impl SteerInput {
    pub(super) fn take_client(&mut self, client_id: &str) -> Option<PendingSteer> {
        if self
            .replacement()
            .is_some_and(|replacement| replacement.appended.client_id == client_id)
        {
            self.abort_replacement()
        } else if self
            .replacement()
            .is_some_and(|replacement| replacement.previous.client_id == client_id)
        {
            let Self::Replacing {
                replacement,
                interrupt,
            } = std::mem::take(self)
            else {
                unreachable!()
            };
            *self = Self::Pending {
                steer: Box::new(replacement.appended),
                interrupt,
            };
            Some(replacement.previous)
        } else if self.client_id() == Some(client_id) {
            self.take()
        } else {
            None
        }
    }

    pub(super) fn as_ref(&self) -> Option<Cow<'_, PendingSteer>> {
        match self {
            Self::Idle => None,
            Self::Pending { steer, .. } => Some(Cow::Borrowed(steer)),
            Self::Replacing { replacement, .. } => Some(Cow::Owned(
                replacement.previous.append(&replacement.appended),
            )),
        }
    }

    pub(crate) fn client_id(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Pending { steer, .. } => Some(&steer.client_id),
            Self::Replacing { replacement, .. } => Some(&replacement.previous.client_id),
        }
    }

    pub(super) fn is_some(&self) -> bool {
        !matches!(self, Self::Idle)
    }
    pub(super) fn is_none(&self) -> bool {
        !self.is_some()
    }
    pub(super) fn take(&mut self) -> Option<PendingSteer> {
        std::mem::take(self).as_ref().map(Cow::into_owned)
    }
    pub(super) fn set(&mut self, steer: Option<PendingSteer>) {
        let interrupt = self.interrupt();
        *self = steer.map_or(Self::Idle, |steer| Self::Pending {
            steer: Box::new(steer),
            interrupt,
        });
    }
    pub(super) fn replacement(&self) -> Option<&PendingSteerReplacement> {
        match self {
            Self::Replacing { replacement, .. } => Some(replacement),
            _ => None,
        }
    }
    pub(super) fn replace(&mut self, replacement: PendingSteerReplacement) {
        let interrupt = self.interrupt();
        *self = Self::Replacing {
            replacement: Box::new(replacement),
            interrupt,
        };
    }
    pub(super) fn interrupt(&self) -> SteerInterrupt {
        match self {
            Self::Idle => SteerInterrupt::RestoreDraft,
            Self::Pending { interrupt, .. } | Self::Replacing { interrupt, .. } => *interrupt,
        }
    }
    pub(super) fn set_interrupt(&mut self, value: SteerInterrupt) {
        match self {
            Self::Idle => {}
            Self::Pending { interrupt, .. } | Self::Replacing { interrupt, .. } => {
                *interrupt = value
            }
        }
    }
    pub(super) fn finish_replacement(&mut self, replaced: bool) -> bool {
        if !matches!(self, Self::Replacing { .. }) {
            return false;
        }
        let Self::Replacing {
            replacement,
            interrupt,
        } = std::mem::take(self)
        else {
            return false;
        };
        let steer = if replaced {
            replacement.previous.append(&replacement.appended)
        } else {
            replacement.appended
        };
        *self = Self::Pending {
            steer: Box::new(steer),
            interrupt,
        };
        true
    }
    pub(super) fn abort_replacement(&mut self) -> Option<PendingSteer> {
        if !matches!(self, Self::Replacing { .. }) {
            return None;
        }
        let Self::Replacing {
            replacement,
            interrupt,
        } = std::mem::take(self)
        else {
            unreachable!()
        };
        *self = Self::Pending {
            steer: Box::new(replacement.previous),
            interrupt,
        };
        Some(replacement.appended)
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputQueueState {
    pub(super) queued_user_messages: VecDeque<QueuedUserMessage>,
    pub(super) rejected_steers_queue: VecDeque<QueuedUserMessage>,
    pub(super) turn: UserTurnInput,
    pub(super) pending_steer: SteerInput,
    pub(super) rate_limit_recovery_pending: bool,
    pub(crate) recovered_queue: bool,
}

impl InputQueueState {
    /// Release only a definitively unsent non-message start, never a submitted prompt.
    pub(crate) fn cancel_unsent_operation(&mut self) -> bool {
        if !matches!(
            self.turn,
            UserTurnInput::AwaitingStart(PendingTurnStart::Operation)
        ) {
            return false;
        }
        self.turn = UserTurnInput::Idle;
        true
    }
}

impl InputQueueState {
    pub(crate) fn has_unconfirmed_messages(&self) -> bool {
        self.queued_user_messages
            .iter()
            .any(|message| matches!(message.delivery, MessageDelivery::Unconfirmed(_)))
    }

    pub(super) fn has_queued_follow_up_messages(&self) -> bool {
        !self.rejected_steers_queue.is_empty() || !self.queued_user_messages.is_empty()
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn preview(&self) -> PendingInputPreview {
        PendingInputPreview {
            queued_messages: self
                .queued_user_messages
                .iter()
                .map(|message| user_message_preview_text(message, Some(&message.history_record)))
                .collect(),
            pending_steer: self.pending_steer.as_ref().map(|steer| {
                user_message_preview_text(&steer.user_message, Some(&steer.history_record))
            }),
            rejected_steers: self
                .rejected_steers_queue
                .iter()
                .map(|message| user_message_preview_text(message, Some(&message.history_record)))
                .collect(),
        }
    }

    pub(crate) fn discard_pending_steer(&mut self, client_id: &str) -> bool {
        if self.pending_steer.client_id() != Some(client_id) {
            return false;
        }
        self.pending_steer.set(None);
        true
    }
}

mod legacy;
