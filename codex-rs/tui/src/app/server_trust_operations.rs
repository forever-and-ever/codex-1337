use super::*;
use crate::hooks_rpc::{HookTrustUpdate, write_hook_trusts_typed};
use crate::onboarding::onboarding_screen::DirectoryTrustConsent;
use codex_app_server_client::{AppServerRequestHandle, TypedRequestError};

enum TrustConsent {
    Resume(ResumeTrustConsent),
    DirectoryChange(directory::DirectoryTrustAdmission),
    Dashboard(crate::app::agents_overview::trust::DashboardTrustAdmission),
}

pub(super) enum ResumeTrustConsent {
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
            TrustConsent::Resume(ResumeTrustConsent::Directory {
                consent: DirectoryTrustConsent::Path(path),
                ..
            }) => path.display().to_string(),
            TrustConsent::Resume(ResumeTrustConsent::Directory {
                consent: DirectoryTrustConsent::Key(key),
                ..
            }) => key.clone(),
            TrustConsent::Resume(ResumeTrustConsent::Hooks { context, .. }) => context.hook_scope(),
            TrustConsent::Dashboard(consent) => consent.scope(),
            TrustConsent::DirectoryChange(consent) => consent.scope(),
        }
    }

    pub(in crate::app::turn_submissions) fn label(&self) -> &'static str {
        match &*self.consent {
            TrustConsent::Resume(ResumeTrustConsent::Directory { .. }) => "Saving folder trust",
            TrustConsent::Resume(ResumeTrustConsent::Hooks { .. }) => "Trusting hooks",
            TrustConsent::Dashboard(consent) => consent.label(),
            TrustConsent::DirectoryChange(_) => "Saving folder trust",
        }
    }

    pub(in crate::app::turn_submissions) async fn send(
        self,
        handle: AppServerRequestHandle,
        id: Uuid,
    ) -> Result<OperationReceipt, TypedRequestError> {
        match &*self.consent {
            TrustConsent::Resume(ResumeTrustConsent::Directory { consent, .. }) => {
                consent.clone().send(handle, id).await?
            }
            TrustConsent::Resume(ResumeTrustConsent::Hooks { updates, .. }) => {
                write_hook_trusts_typed(handle, updates.clone()).await?;
            }
            TrustConsent::Dashboard(consent) => consent.send(handle, id).await?,
            TrustConsent::DirectoryChange(consent) => {
                consent.consent.clone().send(handle, id).await?
            }
        }
        Ok(OperationReceipt::TrustSaved)
    }
}

impl App {
    pub(in crate::app) fn admit_dashboard_trust(
        &mut self,
        server: &AppServerSession,
        consent: crate::app::agents_overview::trust::DashboardTrustAdmission,
    ) {
        self.enqueue_connection_operation(
            server,
            ServerOperation::Connection(ConnectionMutation::Trust(TrustOperation {
                generation: self.server_operations.navigation.generation,
                consent: Arc::new(TrustConsent::Dashboard(consent)),
            })),
        );
    }

    pub(in crate::app) fn pumped_configuration_ready(&mut self) -> bool {
        let Some(lane) = self.server_operations.lanes.get(&OperationLane::Connection) else {
            return true;
        };
        self.add_agents_overview_error(if matches!(lane.state, LaneState::Uncertain) {
            "An earlier configuration outcome is unknown. Close and reopen this TUI to reread settings; the earlier request may still complete."
        } else {
            "Wait for the pending configuration change before starting a session."
        }.into());
        false
    }

    #[cfg(test)]
    pub(in crate::app) fn pending_configuration_is_uncertain(&self) -> bool {
        self.server_operations
            .lanes
            .get(&OperationLane::Connection)
            .is_some_and(|lane| matches!(lane.state, LaneState::Uncertain))
    }

    fn transfer_trust_consent(
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

    pub(super) fn transfer_navigation_trust(
        &mut self,
        server: &AppServerSession,
        operation: PendingNavigation,
        consent: ResumeTrustConsent,
    ) {
        self.transfer_trust_consent(server, operation, TrustConsent::Resume(consent));
    }
    pub(super) fn transfer_directory_trust(
        &mut self,
        server: &AppServerSession,
        operation: PendingNavigation,
        consent: directory::DirectoryTrustAdmission,
    ) {
        self.transfer_trust_consent(server, operation, TrustConsent::DirectoryChange(consent));
    }

    pub(in crate::app::turn_submissions) async fn continue_confirmed_trust(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        completion: &ControlCompletion,
    ) -> Result<AppRunControl> {
        if self.reconnect.offline
            || completion.epoch != self.server_operations.epoch
            || completion.scope != OperationScope::Connection
            || !matches!(
                &completion.outcome,
                SubmissionOutcome::Operation(Ok(OperationReceipt::TrustSaved))
            )
        {
            return Ok(AppRunControl::Continue);
        }
        let Some(lane) = self.server_operations.lanes.get(&OperationLane::Connection) else {
            return Ok(AppRunControl::Continue);
        };
        if lane.active.id != completion.id || lane.active.epoch != completion.epoch {
            return Ok(AppRunControl::Continue);
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
            return Ok(AppRunControl::Continue);
        }
        match &*consent {
            TrustConsent::DirectoryChange(consent) => {
                let pending = PendingNavigation {
                    id: lane.active.id,
                    order: lane.active.order,
                    epoch: lane.active.epoch,
                    generation: operation.generation,
                    intent: NavigationIntent::Transitioning,
                    state: LaneState::Queued,
                };
                self.continue_directory_trust_saved(server, pending, consent)
                    .await;
                Ok(AppRunControl::Continue)
            }
            TrustConsent::Resume(consent) => {
                let pending = PendingNavigation {
                    id: lane.active.id,
                    order: lane.active.order,
                    epoch: lane.active.epoch,
                    generation: operation.generation,
                    intent: NavigationIntent::Transitioning,
                    state: LaneState::Queued,
                };
                self.continue_resume_trust_saved(server, pending, consent)
                    .await?;
                Ok(AppRunControl::Continue)
            }
            TrustConsent::Dashboard(consent) => {
                Box::pin(self.continue_confirmed_dashboard_trust(tui, server, consent)).await
            }
        }
    }
}
