//! Threadless navigation shares operation admission, lifecycle and retirement, not a sentinel thread.

use super::*;

#[derive(Default)]
pub(super) struct NavigationOwner {
    current: Option<PendingNavigation>,
    retired: Vec<PendingNavigation>,
    generation: u64,
}

struct PendingNavigation {
    id: Uuid,
    order: u64,
    epoch: u64,
    generation: u64,
    intent: NavigationIntent,
    state: LaneState,
}

enum NavigationIntent {
    Picker(Arc<PickerPreparation>),
    ClosePicker,
}

struct PickerPreparation {
    config: Config,
    target: AppServerTarget,
    cli_kv_overrides: Vec<(String, TomlValue)>,
    loader_overrides: LoaderOverrides,
    state_db: Option<StateDbHandle>,
    environment_manager: Arc<EnvironmentManager>,
}

pub(crate) struct NavigationCompletion {
    id: Uuid,
    order: u64,
    epoch: u64,
    generation: u64,
    receipt: NavigationReceipt,
}

enum NavigationReceipt {
    Picker(Result<AppServerSession, String>),
    PickerClosed,
}

impl std::fmt::Debug for NavigationCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NavigationCompletion")
            .field("id", &self.id)
            .field("epoch", &self.epoch)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl NavigationOwner {
    pub(super) fn is_empty(&self) -> bool {
        self.current.is_none() && self.retired.is_empty()
    }

    pub(super) fn precedes_barrier(&self, order: u64, epoch: u64) -> bool {
        self.current
            .iter()
            .chain(&self.retired)
            .any(|operation| operation.epoch == epoch && operation.order < order)
    }

    pub(super) fn preview(&self) -> Option<NavigationPreview> {
        if let Some(operation) = &self.current {
            Some(NavigationPreview::Preparing { id: operation.id })
        } else if !self.retired.is_empty() {
            Some(NavigationPreview::Cleaning)
        } else {
            None
        }
    }

    pub(super) fn retire_current(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("navigation generation exhausted");
        if let Some(operation) = self.current.take()
            && !matches!(operation.state, LaneState::Queued)
        {
            // Do not abort a resource-producing request: its late session still needs an owner.
            self.retired.push(operation);
        }
    }

    pub(super) fn cancel(&mut self, id: Uuid) -> bool {
        if self
            .current
            .as_ref()
            .is_none_or(|operation| operation.id != id)
        {
            return false;
        }
        self.retire_current();
        true
    }
}

impl App {
    pub(in crate::app) fn supersede_pending_navigation(&mut self) {
        self.server_operations.navigation.retire_current();
        self.refresh_submission_preview();
    }

    pub(in crate::app) fn enqueue_picker_preparation(&mut self, server: &AppServerSession) {
        self.server_operations.navigation.retire_current();
        let order = self.server_operations.next_order;
        self.server_operations.next_order =
            order.checked_add(1).expect("operation sequence exhausted");
        let intent = NavigationIntent::Picker(Arc::new(PickerPreparation {
            config: self.config.clone(),
            target: self.app_server_target.clone(),
            cli_kv_overrides: self.cli_kv_overrides.clone(),
            loader_overrides: self.loader_overrides.clone(),
            state_db: self.state_db.clone(),
            environment_manager: Arc::clone(&self.environment_manager),
        }));
        self.server_operations.navigation.current = Some(PendingNavigation {
            id: Uuid::new_v4(),
            order,
            epoch: self.server_operations.epoch,
            generation: self.server_operations.navigation.generation,
            intent,
            state: LaneState::Queued,
        });
        self.drive_server_operations(server);
        self.refresh_submission_preview();
    }

    pub(super) fn drive_navigation_operation(&mut self) {
        let barrier = self
            .server_operations
            .lanes
            .get(&OperationLane::Connection)
            .map(|lane| lane.active.order);
        let Some(operation) = self.server_operations.navigation.current.as_mut() else {
            return;
        };
        if !matches!(operation.state, LaneState::Queued)
            || barrier.is_some_and(|order| order < operation.order)
        {
            return;
        }
        let NavigationIntent::Picker(preparation) = &operation.intent else {
            unreachable!("cleanup is dispatched with its owned session");
        };
        let preparation = Arc::clone(preparation);
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        let events = self.app_event_tx.clone();
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            let result = crate::start_app_server_for_picker(
                &preparation.config,
                &preparation.target,
                preparation.cli_kv_overrides.clone(),
                preparation.loader_overrides.clone(),
                preparation.state_db.clone(),
                Arc::clone(&preparation.environment_manager),
            )
            .await
            .map_err(|error| error.to_string());
            events.send(AppEvent::ServerNavigationPrepared(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::Picker(result),
                },
            )));
        })));
    }

    pub(in crate::app) async fn finish_navigation_operation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        completion: NavigationCompletion,
    ) -> Result<AppRunControl> {
        let matching = self
            .server_operations
            .navigation
            .current
            .as_ref()
            .is_some_and(|operation| {
                operation.id == completion.id
                    && operation.epoch == completion.epoch
                    && operation.generation == completion.generation
            })
            && completion.epoch == self.server_operations.epoch;
        let operation = if matching {
            self.server_operations.navigation.current.take()
        } else {
            self.server_operations
                .navigation
                .retired
                .iter()
                .position(|operation| operation.id == completion.id)
                .map(|index| self.server_operations.navigation.retired.remove(index))
        };
        let Some(mut operation) = operation else {
            // Even a queued envelope outlives retirement: successful session ownership must not vanish.
            if let NavigationReceipt::Picker(Ok(session)) = completion.receipt {
                self.close_stale_picker_session(
                    completion.id,
                    completion.order,
                    completion.epoch,
                    completion.generation,
                    session,
                );
            }
            return Ok(AppRunControl::Continue);
        };
        match completion.receipt {
            NavigationReceipt::Picker(Ok(mut picker)) if matching => {
                picker.model_provider_override = self.harness_overrides.model_provider.clone();
                self.drive_server_operations(server);
                self.refresh_submission_preview();
                let selection =
                    crate::resume_picker::run_resume_picker_from_existing_session_with_app_server(
                        crate::uses_remote_workspace_or_environment(
                            &self.app_server_target,
                            &self.environment_manager,
                        ),
                        tui,
                        &self.config,
                        &self.local_settings,
                        false,
                        false,
                        picker,
                        server.request_handle(),
                        self.primary_thread_id
                            .or(self.current_displayed_thread_id()),
                    )
                    .await;
                match selection {
                    Ok(selection) => {
                        return self
                            .apply_resume_picker_selection(tui, server, selection)
                            .await;
                    }
                    Err(error) => self.add_session_picker_error(format!(
                        "Failed to open session picker: {error}"
                    )),
                }
            }
            NavigationReceipt::Picker(Ok(picker)) => {
                operation.intent = NavigationIntent::ClosePicker;
                self.spawn_picker_cleanup(&mut operation, picker);
                self.server_operations.navigation.retired.push(operation);
            }
            NavigationReceipt::Picker(Err(error)) if matching => {
                self.add_session_picker_error(format!(
                    "Failed to start TUI session picker: {error}"
                ));
                self.chat_widget.maybe_send_next_queued_input();
            }
            NavigationReceipt::Picker(Err(_)) | NavigationReceipt::PickerClosed => {}
        }
        self.drive_server_operations(server);
        self.refresh_submission_preview();
        Ok(AppRunControl::Continue)
    }

    fn close_stale_picker_session(
        &mut self,
        id: Uuid,
        order: u64,
        epoch: u64,
        generation: u64,
        session: AppServerSession,
    ) {
        let mut operation = PendingNavigation {
            id,
            order,
            epoch,
            generation,
            intent: NavigationIntent::ClosePicker,
            state: LaneState::Queued,
        };
        self.spawn_picker_cleanup(&mut operation, session);
        self.server_operations.navigation.retired.push(operation);
    }

    fn spawn_picker_cleanup(&self, operation: &mut PendingNavigation, session: AppServerSession) {
        let events = self.app_event_tx.clone();
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            if let Err(error) = session.shutdown().await {
                tracing::warn!(%error, "failed to close superseded picker session");
            }
            events.send(AppEvent::ServerNavigationPrepared(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::PickerClosed,
                },
            )));
        })));
    }
}
