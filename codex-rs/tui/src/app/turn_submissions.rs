//! One ordered submission lane per thread. Only App mutates the ledger; widgets receive receipts
//! and immutable previews. An uncertain predecessor retires its whole lane into unsent drafts.

use super::thread_delivery::LocalThreadCompletion;
use super::*;
use crate::app_command::PendingSteerCancelAction;
use crate::chatwidget::PendingSteer;
use crate::chatwidget::UserMessage;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::TurnStartParams;
use std::collections::VecDeque;
use tokio_util::task::AbortOnDropHandle;
use uuid::Uuid;

#[path = "turn_submission_controller.rs"]
mod controller;
#[path = "turn_submission_worker.rs"]
mod worker;

#[derive(Default)]
pub(super) struct TurnSubmissions {
    lanes: HashMap<ThreadId, SubmissionLane>,
}

struct SubmissionLane {
    active: PendingSubmission,
    task: AbortOnDropHandle<()>,
    waiting: VecDeque<PendingSubmission>,
}

struct PendingSubmission {
    id: Uuid,
    request: SubmissionRequest,
    buffering: Option<codex_app_server_protocol::ModelSafetyBufferingUpdatedNotification>,
}

enum SubmissionRequest {
    Turn(Box<ReadyTurn>),
    Cancel {
        client_id: String,
        action: PendingSteerCancelAction,
        draft: Option<Box<SubmissionDraft>>,
    },
}

struct ReadyTurn {
    origin: TurnOrigin,
    params: TurnStartParams,
    draft: SubmissionDraft,
}

enum TurnOrigin {
    User(Box<UserTurnReplay>),
    Subagent { expected_turn_id: Option<String> },
}

struct UserTurnReplay(AppCommand);

impl UserTurnReplay {
    fn new(command: AppCommand) -> Result<Self> {
        if matches!(command, AppCommand::UserTurn { .. }) {
            Ok(Self(command))
        } else {
            Err(color_eyre::eyre::eyre!(
                "submission replay must be a user turn"
            ))
        }
    }

    fn command(&self) -> &AppCommand {
        &self.0
    }

    fn replacement(&self) -> Option<&crate::app_command::PendingSteerReplacement> {
        match &self.0 {
            AppCommand::UserTurn {
                pending_steer_replacement,
                ..
            } => pending_steer_replacement.as_ref(),
            _ => None,
        }
    }

    fn replace_waiting(
        &mut self,
        draft: &SubmissionDraft,
        input: &[codex_app_server_protocol::UserInput],
    ) {
        if let AppCommand::UserTurn {
            client_user_message_id,
            items,
            pending_steer_replacement,
            ..
        } = &mut self.0
        {
            *client_user_message_id = draft.client_id.clone();
            *items = input.to_vec();
            *pending_steer_replacement = None;
        }
    }
}

enum WorkerRequest {
    Turn {
        command: WorkerCommand,
        params: Box<TurnStartParams>,
    },
    Cancel {
        client_id: String,
    },
}

enum WorkerCommand {
    User {
        replacement: Option<crate::app_command::PendingSteerReplacement>,
    },
    Subagent {
        expected_turn_id: Option<String>,
    },
}

#[derive(Clone)]
pub(crate) struct SubmissionDraft {
    pub(crate) client_id: String,
    pub(crate) message: UserMessage,
    pub(crate) origin: DraftOrigin,
}

#[derive(Clone)]
pub(crate) enum DraftOrigin {
    Message {
        source: crate::chatwidget::UserMessageSource,
    },
    Append {
        previous: Box<PendingSteer>,
        appended: Box<PendingSteer>,
    },
}

#[derive(Clone, PartialEq)]
pub(crate) enum DraftDisposition {
    Edit,
    Remove,
    Recover { uncertain_id: Option<String> },
}

impl From<PendingSteerCancelAction> for DraftDisposition {
    fn from(action: PendingSteerCancelAction) -> Self {
        match action {
            PendingSteerCancelAction::Edit => Self::Edit,
            PendingSteerCancelAction::Remove => Self::Remove,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SubmissionPreviewEntry {
    Message { id: Uuid, text: String },
    Cancellation { text: &'static str },
}

impl SubmissionPreviewEntry {
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Message { text, .. } => text,
            Self::Cancellation { text } => text,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum SubmissionPreview {
    #[default]
    Idle,
    Sending {
        active: SubmissionPreviewEntry,
        waiting: Vec<SubmissionPreviewEntry>,
    },
}

#[derive(Debug)]
pub(crate) enum SubmissionEvent {
    Edit {
        thread_id: ThreadId,
        id: Uuid,
        action: PendingSteerCancelAction,
    },
    Retire,
}

#[derive(Debug)]
pub(super) enum SubmissionOutcome {
    Replaced,
    Finished {
        replacement: ReplacementContinuation,
        observations: Vec<TurnObservation>,
        result: Result<SubmissionReceipt, SubmissionFailure>,
    },
}

#[derive(Debug)]
pub(super) enum ReplacementContinuation {
    NoReceipt,
    Missed,
}

#[derive(Debug)]
pub(super) enum TurnObservation {
    Missing { expected: String },
    Mismatch { expected: String, actual: String },
}

#[derive(Debug)]
pub(super) enum SubmissionReceipt {
    Steered,
    Started {
        turn_id: String,
        previous_latest: Option<String>,
    },
    Cancelled(bool),
    Deferred(String),
}

#[derive(Debug)]
pub(super) enum SubmissionFailure {
    Rejected(TypedRequestError),
    Uncertain(TypedRequestError),
    TimedOut,
}

impl PendingSubmission {
    fn preview(&self) -> SubmissionPreviewEntry {
        match &self.request {
            SubmissionRequest::Turn(turn) => SubmissionPreviewEntry::Message {
                id: self.id,
                text: history_cell::sanitize_user_text(turn.draft.message.text.clone().into())
                    .into_owned(),
            },
            SubmissionRequest::Cancel { action, .. } => SubmissionPreviewEntry::Cancellation {
                text: match action {
                    PendingSteerCancelAction::Edit => "Editing pending steer",
                    PendingSteerCancelAction::Remove => "Cancelling pending steer",
                },
            },
        }
    }

    fn client_id(&self) -> &str {
        match &self.request {
            SubmissionRequest::Turn(turn) => &turn.draft.client_id,
            SubmissionRequest::Cancel { client_id, .. } => client_id,
        }
    }

    fn into_draft(self) -> Option<SubmissionDraft> {
        match self.request {
            SubmissionRequest::Turn(turn) => Some(turn.draft),
            SubmissionRequest::Cancel { draft, .. } => draft.map(|draft| *draft),
        }
    }

    fn worker_request(&self) -> WorkerRequest {
        match &self.request {
            SubmissionRequest::Turn(turn) => WorkerRequest::Turn {
                command: match &turn.origin {
                    TurnOrigin::User(replay) => WorkerCommand::User {
                        replacement: replay.replacement().cloned(),
                    },
                    TurnOrigin::Subagent { expected_turn_id } => WorkerCommand::Subagent {
                        expected_turn_id: expected_turn_id.clone(),
                    },
                },
                params: Box::new(turn.params.clone()),
            },
            SubmissionRequest::Cancel { client_id, .. } => WorkerRequest::Cancel {
                client_id: client_id.clone(),
            },
        }
    }
}

impl TurnSubmissions {
    pub(super) fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    fn preview(&self, thread_id: Option<ThreadId>) -> SubmissionPreview {
        match thread_id.and_then(|id| self.lanes.get(&id)) {
            Some(lane) => SubmissionPreview::Sending {
                active: lane.active.preview(),
                waiting: lane
                    .waiting
                    .iter()
                    .map(PendingSubmission::preview)
                    .collect(),
            },
            None => SubmissionPreview::Idle,
        }
    }
}

fn recoverable_drafts(entries: impl Iterator<Item = PendingSubmission>) -> Vec<SubmissionDraft> {
    let mut drafts: Vec<SubmissionDraft> = Vec::new();
    for entry in entries {
        let cancellation = matches!(entry.request, SubmissionRequest::Cancel { .. });
        let Some(draft) = entry.into_draft() else {
            continue;
        };
        if cancellation
            && drafts
                .iter()
                .any(|previous| previous.client_id == draft.client_id)
        {
            continue;
        }
        // A cancellation can follow an in-flight append to an older steer. Its snapshot belongs
        // before the appended text, not after it; on a successful append this snapshot remains
        // owned by the cancellation until its own acknowledgement arrives.
        let index = drafts.iter().position(|previous| {
            matches!(&previous.origin, DraftOrigin::Append { previous: steer, .. } if steer.client_id == draft.client_id)
        }).unwrap_or(drafts.len());
        drafts.insert(index, draft);
    }
    drafts
}
