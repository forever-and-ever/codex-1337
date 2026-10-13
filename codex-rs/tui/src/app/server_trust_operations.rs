use super::*;
use crate::hooks_rpc::{HookTrustUpdate, write_hook_trusts_typed};
use crate::onboarding::onboarding_screen::DirectoryTrustConsent;
use codex_app_server_client::{AppServerRequestHandle, TypedRequestError};

pub(super) enum TrustConsent {
    Directory {
        consent: DirectoryTrustConsent,
        context: Box<resume::ResumeContext>,
        plan: crate::onboarding::onboarding_screen::DirectoryTrustPreparation,
    },
    Hooks {
        updates: Vec<HookTrustUpdate>,
        context: Box<resume::ResumeContext>,
    },
}

#[derive(Clone)]
pub(in crate::app) struct TrustOperation {
    generation: u64,
    consent: Arc<TrustConsent>,
}

impl TrustOperation {
    pub(in crate::app::turn_submissions) fn scope(&self) -> String {
        match &*self.consent {
            TrustConsent::Directory {
                consent: DirectoryTrustConsent::Path(path),
                ..
            } => path.display().to_string(),
            TrustConsent::Directory {
                consent: DirectoryTrustConsent::Key(key),
                ..
            } => key.clone(),
            TrustConsent::Hooks { context, .. } => context.hook_scope(),
        }
    }

    pub(in crate::app::turn_submissions) fn label(&self) -> &'static str {
        match &*self.consent {
            TrustConsent::Directory { .. } => "Saving folder trust",
            TrustConsent::Hooks { .. } => "Trusting hooks",
        }
    }

    pub(in crate::app::turn_submissions) async fn send(
        self,
        handle: AppServerRequestHandle,
        id: Uuid,
    ) -> Result<OperationReceipt, TypedRequestError> {
        match &*self.consent {
            TrustConsent::Directory { consent, .. } => consent.clone().send(handle, id).await?,
            TrustConsent::Hooks { updates, .. } => {
                write_hook_trusts_typed(handle, updates.clone()).await?;
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
        consent: TrustConsent,
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
                    generation,
                    consent: Arc::new(consent),
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
        if self.reconnect.offline
            || completion.epoch != self.server_operations.epoch
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
        let consent = Arc::clone(&operation.consent);
        if operation.generation != self.server_operations.navigation.generation
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
            generation: operation.generation,
            intent: NavigationIntent::Transitioning,
            state: LaneState::Queued,
        };
        self.continue_resume_trust_saved(server, pending, &consent)
            .await
    }
}
