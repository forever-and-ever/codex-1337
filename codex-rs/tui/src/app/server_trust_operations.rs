use super::*;
use crate::hooks_rpc::{HookTrustUpdate, write_hook_trusts_typed};
use crate::onboarding::onboarding_screen::DirectoryTrustConsent;
use codex_app_server_client::{AppServerRequestHandle, TypedRequestError};

#[derive(Clone)]
pub(in crate::app) enum TrustWrite {
    Directory(DirectoryTrustConsent),
    Hooks(Vec<HookTrustUpdate>),
}

#[derive(Clone)]
pub(in crate::app) struct TrustOperation {
    write: TrustWrite,
    continuation: Arc<resume::ResumeTrustContinuation>,
}

impl TrustOperation {
    pub(in crate::app::turn_submissions) fn scope(&self) -> String {
        match &self.write {
            TrustWrite::Directory(DirectoryTrustConsent::Path(path)) => path.display().to_string(),
            TrustWrite::Directory(DirectoryTrustConsent::Key(key)) => key.clone(),
            TrustWrite::Hooks(_) => self.continuation.next.hook_scope(),
        }
    }

    pub(in crate::app::turn_submissions) fn label(&self) -> &'static str {
        match self.write {
            TrustWrite::Directory(_) => "Saving folder trust",
            TrustWrite::Hooks(_) => "Trusting hooks",
        }
    }

    pub(in crate::app::turn_submissions) async fn send(
        self,
        handle: AppServerRequestHandle,
        id: Uuid,
    ) -> Result<OperationReceipt, TypedRequestError> {
        match self.write {
            TrustWrite::Directory(consent) => consent.send(handle, id).await?,
            TrustWrite::Hooks(updates) => {
                write_hook_trusts_typed(handle, updates).await?;
            }
        }
        Ok(OperationReceipt::TrustSaved)
    }
}

impl App {
    pub(super) fn transfer_navigation_trust(
        &mut self,
        server: &AppServerSession,
        operation: PendingNavigation,
        write: TrustWrite,
        next: resume::ResumeTrustNext,
    ) {
        let PendingNavigation {
            id,
            order,
            epoch,
            generation,
            ..
        } = operation;
        let active = PendingSubmission {
            id,
            order,
            epoch,
            origin: OperationOrigin::Connection,
            request: SubmissionRequest::Operation(Box::new(ServerOperation::Connection(
                ConnectionMutation::Trust(TrustOperation {
                    write,
                    continuation: Arc::new(resume::ResumeTrustContinuation { generation, next }),
                }),
            ))),
            buffering: None,
        };
        let mut waiting = VecDeque::new();
        if let Some(lane) = self
            .server_operations
            .lanes
            .remove(&OperationLane::Connection)
        {
            assert!(
                matches!(lane.state, LaneState::Queued) && lane.active.order > order,
                "navigation trust transfer must precede a definitively unsent connection head"
            );
            waiting = lane.waiting;
            waiting.push_front(lane.active);
        }
        self.server_operations.lanes.insert(
            OperationLane::Connection,
            SubmissionLane {
                active,
                state: LaneState::Queued,
                waiting,
            },
        );
        self.drive_server_operations(server);
        self.refresh_submission_preview();
    }

    pub(in crate::app::turn_submissions) async fn continue_confirmed_trust(
        &mut self,
        server: &mut AppServerSession,
        completion: &ControlCompletion,
    ) -> Result<()> {
        if completion.epoch != self.server_operations.epoch
            || completion.scope != OperationScope::Connection
            || !matches!(
                &completion.outcome,
                SubmissionOutcome::Operation(Ok(OperationReceipt::TrustSaved))
            )
        {
            return Ok(());
        }
        let Some(lane) = self.server_operations.lanes.get(&OperationLane::Connection) else {
            return Ok(());
        };
        if lane.active.id != completion.id || lane.active.epoch != completion.epoch {
            return Ok(());
        }
        let SubmissionRequest::Operation(operation) = &lane.active.request else {
            unreachable!("trust receipt without operation payload");
        };
        let ServerOperation::Connection(ConnectionMutation::Trust(operation)) = &**operation else {
            unreachable!("trust receipt without consent");
        };
        let continuation = Arc::clone(&operation.continuation);
        if continuation.generation != self.server_operations.navigation.generation
            || self.server_operations.navigation.current.is_some()
        {
            self.chat_widget.add_info_message(
                format!(
                    "Trust saved for {}. The superseded navigation was not resumed.",
                    operation.scope()
                ),
                None,
            );
            return Ok(());
        }
        let pending = PendingNavigation {
            id: lane.active.id,
            order: lane.active.order,
            epoch: lane.active.epoch,
            generation: continuation.generation,
            intent: NavigationIntent::Transitioning,
            state: LaneState::Queued,
        };
        self.continue_resume_trust_saved(server, pending, continuation.next.clone())
            .await
    }
}
