//! One operation owner with ordered thread lanes and a connection-wide configuration barrier.
//! Only App mutates the ledger; workers own requests and widgets receive immutable previews.
//! Turn failures preserve draft recovery; uncertain mutations retain their ordering fence.

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

#[path = "server_operation_requests.rs"]
mod requests;
use requests::OperationReceipt;
pub(in crate::app) use requests::ThreadOperation;

#[path = "server_control_operations.rs"]
mod control;
pub(in crate::app) use control::ControlOperation;
pub(in crate::app) use control::GoalStop;

#[derive(Debug)]
pub(in crate::app) struct ControlCompletion {
    pub(in crate::app) thread_id: ThreadId,
    pub(in crate::app) id: Uuid,
    pub(in crate::app) epoch: u64,
    pub(in crate::app) outcome: SubmissionOutcome,
}

#[path = "server_detach_operations.rs"]
mod detach;

pub(in crate::app) use detach::DetachScope;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum OperationLane {
    Thread(ThreadId),
    Control(ThreadId),
    Exit,
    Connection,
}

pub(in crate::app) use requests::CatalogRequest;
pub(in crate::app) use requests::ConnectionMutation;
pub(in crate::app) use requests::ThreadMutation;

#[path = "server_navigation_operations.rs"]
mod navigation;
pub(crate) use navigation::NavigationCompletion;

pub(super) struct ServerOperations {
    lanes: HashMap<OperationLane, SubmissionLane>,
    navigation: navigation::NavigationOwner,
    next_order: u64,
    epoch: u64,
    control_tx: mpsc::UnboundedSender<ControlCompletion>,
    pub(in crate::app) control_rx: mpsc::UnboundedReceiver<ControlCompletion>,
}

impl Default for ServerOperations {
    fn default() -> Self {
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        Self {
            lanes: HashMap::new(),
            navigation: navigation::NavigationOwner::default(),
            next_order: 0,
            epoch: 0,
            control_tx,
            control_rx,
        }
    }
}

struct SubmissionLane {
    active: PendingSubmission,
    state: LaneState,
    waiting: VecDeque<PendingSubmission>,
}

enum LaneState {
    Queued,
    Running(AbortOnDropHandle<()>),
    Uncertain,
}

struct PendingSubmission {
    id: Uuid,
    order: u64,
    epoch: u64,
    thread_id: ThreadId,
    store: Arc<Mutex<ThreadEventStore>>,
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
    Operation(Box<ThreadOperation>),
    Continuation {
        review: Arc<crate::chatwidget::MisalignmentReview>,
        params: Box<TurnStartParams>,
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
    Operation {
        id: Uuid,
        operation: Box<ThreadOperation>,
    },
    Continuation {
        params: Box<TurnStartParams>,
        review: Arc<crate::chatwidget::MisalignmentReview>,
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
    Operation { id: Uuid, text: &'static str },
}

impl SubmissionPreviewEntry {
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Message { text, .. } => text,
            Self::Cancellation { text } | Self::Operation { text, .. } => text,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NavigationPreview {
    Preparing { id: Uuid },
    Cleaning,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum SubmissionPreview {
    #[default]
    Idle,
    Navigation {
        navigation: NavigationPreview,
        submissions: Box<SubmissionPreview>,
    },
    Sending {
        active: SubmissionPreviewEntry,
        waiting: Vec<SubmissionPreviewEntry>,
    },
}

impl SubmissionPreview {
    pub(crate) fn thread_preview(&self) -> &Self {
        match self {
            Self::Navigation { submissions, .. } => submissions.thread_preview(),
            _ => self,
        }
    }
}

#[derive(Debug)]
pub(crate) enum SubmissionEvent {
    Edit {
        thread_id: ThreadId,
        id: Uuid,
        action: PendingSteerCancelAction,
    },
    CancelNavigation {
        id: Uuid,
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
    Operation(Result<OperationReceipt, SubmissionFailure>),
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
    Continued(codex_app_server_protocol::Turn),
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
            SubmissionRequest::Operation(operation) => SubmissionPreviewEntry::Operation {
                id: self.id,
                text: operation.label(),
            },
            SubmissionRequest::Continuation { .. } => SubmissionPreviewEntry::Operation {
                id: self.id,
                text: "Continuing after review",
            },
            SubmissionRequest::Cancel { action, .. } => SubmissionPreviewEntry::Cancellation {
                text: match action {
                    PendingSteerCancelAction::Edit => "Editing pending steer",
                    PendingSteerCancelAction::Remove => "Cancelling pending steer",
                },
            },
        }
    }

    fn client_id(&self) -> Option<&str> {
        match &self.request {
            SubmissionRequest::Turn(turn) => Some(&turn.draft.client_id),
            SubmissionRequest::Cancel { client_id, .. } => Some(client_id),
            SubmissionRequest::Operation(_) | SubmissionRequest::Continuation { .. } => None,
        }
    }

    fn into_draft(self) -> Option<SubmissionDraft> {
        match self.request {
            SubmissionRequest::Turn(turn) => Some(turn.draft),
            SubmissionRequest::Cancel { draft, .. } => draft.map(|draft| *draft),
            SubmissionRequest::Operation(_) | SubmissionRequest::Continuation { .. } => None,
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
            SubmissionRequest::Operation(operation) => WorkerRequest::Operation {
                id: self.id,
                operation: operation.clone(),
            },
            SubmissionRequest::Continuation { params, review } => WorkerRequest::Continuation {
                params: params.clone(),
                review: Arc::clone(review),
            },
            SubmissionRequest::Cancel { client_id, .. } => WorkerRequest::Cancel {
                client_id: client_id.clone(),
            },
        }
    }
}

impl ServerOperations {
    fn take_unsent(
        &mut self,
        thread_id: ThreadId,
        id: Uuid,
        action: PendingSteerCancelAction,
    ) -> Option<PendingSubmission> {
        let editable = |entry: &PendingSubmission| {
            entry.id == id
                && (matches!(entry.request, SubmissionRequest::Turn(_))
                    || action == PendingSteerCancelAction::Remove)
        };
        let key = self.lanes.iter().find_map(|(key, lane)| {
            if *key != OperationLane::Connection && lane.active.thread_id != thread_id {
                return None;
            }
            ((matches!(lane.state, LaneState::Queued) && editable(&lane.active))
                || lane.waiting.iter().any(&editable))
            .then_some(*key)
        })?;
        let SubmissionLane {
            active,
            state,
            mut waiting,
        } = self
            .lanes
            .remove(&key)
            .expect("selected operation lane disappeared");
        if active.id == id {
            if let Some(next) = waiting.pop_front() {
                self.lanes.insert(
                    key,
                    SubmissionLane {
                        active: next,
                        state: LaneState::Queued,
                        waiting,
                    },
                );
            }
            Some(active)
        } else {
            let index = waiting
                .iter()
                .position(editable)
                .expect("selected queued operation disappeared");
            let removed = waiting.remove(index);
            self.lanes.insert(
                key,
                SubmissionLane {
                    active,
                    state,
                    waiting,
                },
            );
            removed
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.lanes.is_empty() && self.navigation.is_empty()
    }

    pub(in crate::app) fn preview(&self, thread_id: Option<ThreadId>) -> SubmissionPreview {
        let submissions = self.thread_preview(thread_id);
        match self.navigation.preview() {
            Some(navigation) => SubmissionPreview::Navigation {
                navigation,
                submissions: Box::new(submissions),
            },
            None => submissions,
        }
    }

    pub(in crate::app) fn thread_preview(&self, thread_id: Option<ThreadId>) -> SubmissionPreview {
        let Some(thread_id) = thread_id else {
            return SubmissionPreview::Idle;
        };
        let mut entries = Vec::new();
        for key in [
            OperationLane::Thread(thread_id),
            OperationLane::Control(thread_id),
            OperationLane::Connection,
            OperationLane::Exit,
        ] {
            if let Some(lane) = self.lanes.get(&key) {
                entries.push((
                    &lane.active,
                    matches!(lane.state, LaneState::Queued),
                    matches!(lane.state, LaneState::Uncertain),
                ));
                entries.extend(lane.waiting.iter().map(|entry| (entry, true, false)));
            }
        }
        entries.sort_by_key(|(entry, _, _)| entry.order);
        let Some((first, queued, uncertain)) = entries.first() else {
            return SubmissionPreview::Idle;
        };
        let active = if *queued {
            SubmissionPreviewEntry::Cancellation {
                text: "Waiting for earlier work",
            }
        } else if *uncertain {
            SubmissionPreviewEntry::Cancellation {
                text: "Operation outcome unknown; no retry sent",
            }
        } else {
            first.preview()
        };
        let waiting = entries
            .iter()
            .skip(usize::from(!*queued))
            .map(|(entry, _, _)| entry.preview())
            .collect();
        SubmissionPreview::Sending { active, waiting }
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
