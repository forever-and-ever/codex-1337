//! Threadless navigation shares operation admission, lifecycle and retirement, not a sentinel thread.

use super::*;

use futures::FutureExt;

#[path = "server_agent_discovery.rs"]
mod discovery;
#[path = "server_thread_selection.rs"]
mod selection;

use selection::SelectStage;
pub(in crate::app) use selection::SelectionAfter;

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
    Picker,
    Discover(discovery::DiscoveryIntent),
    Select(SelectStage),
    Transitioning,
    PickerDefaults(Box<PickerPreparation>),
    PickerStarting,
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
    PickerDefaults(Result<Option<crate::config_update::EffectiveConfig>, String>),
    Picker(Result<AppServerSession, String>),
    PickerClosed,
    Discovered(discovery::DiscoveryReceipt),
    Selected(selection::SelectionReceipt),
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

    pub(super) fn precedes_thread(&self, thread_id: ThreadId, order: u64, epoch: u64) -> bool {
        self.current.iter().chain(&self.retired).any(|operation| operation.epoch == epoch && operation.order < order && matches!(&operation.intent, NavigationIntent::Select(stage) if stage.thread_id() == thread_id))
    }
    pub(super) fn retire_epoch(&mut self, epoch: u64) {
        self.retired.retain(|operation| {
            operation.epoch == epoch || !matches!(operation.state, LaneState::Uncertain)
        });
    }

    pub(super) fn preview(&self) -> Option<NavigationPreview> {
        if let Some(operation) = &self.current {
            Some(if matches!(operation.state, LaneState::Uncertain) {
                NavigationPreview::Uncertain
            } else {
                NavigationPreview::Preparing { id: operation.id }
            })
        } else if self
            .retired
            .iter()
            .any(|operation| matches!(operation.state, LaneState::Uncertain))
        {
            Some(NavigationPreview::Uncertain)
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
            && !matches!(
                operation.intent,
                NavigationIntent::Discover(_)
                    | NavigationIntent::PickerDefaults(_)
                    | NavigationIntent::Select(
                        SelectStage::Liveness { .. } | SelectStage::Fallback(_)
                    )
            )
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
        self.enqueue_navigation(server, NavigationIntent::Picker);
    }

    fn enqueue_navigation(&mut self, server: &AppServerSession, intent: NavigationIntent) {
        self.server_operations.navigation.retire_current();
        let order = self.server_operations.next_order;
        self.server_operations.next_order =
            order.checked_add(1).expect("operation sequence exhausted");
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

    pub(super) fn drive_navigation_operation(&mut self, server: &AppServerSession) {
        let barrier = self
            .server_operations
            .lanes
            .get(&OperationLane::Connection)
            .map(|lane| lane.active.order);
        let Some(mut operation) = self.server_operations.navigation.current.take() else {
            return;
        };
        let thread_target = match &operation.intent {
            NavigationIntent::Select(stage) => Some(stage.thread_id()),
            _ => None,
        };
        let thread_blocked = thread_target.is_some_and(|thread_id| {
            self.server_operations.navigation.precedes_thread(thread_id, operation.epoch, operation.order)
            || self.server_operations.lanes.iter().any(|(lane, earlier)| earlier.active.order < operation.order && matches!(lane, OperationLane::Thread(id) | OperationLane::Realtime(id) if *id == thread_id))
        });
        if !matches!(operation.state, LaneState::Queued)
            || barrier.is_some_and(|order| order < operation.order)
            || thread_blocked
        {
            self.server_operations.navigation.current = Some(operation);
            return;
        }
        if let NavigationIntent::Select(SelectStage::Liveness { thread_id, .. }) = &operation.intent
        {
            let thread_id = *thread_id;
            self.dispatch_selection_liveness(server, &mut operation, thread_id);
            self.server_operations.navigation.current = Some(operation);
            return;
        }
        if matches!(operation.intent, NavigationIntent::Discover(_)) {
            self.dispatch_discovery(server, &mut operation);
            self.server_operations.navigation.current = Some(operation);
            return;
        }
        assert!(
            matches!(operation.intent, NavigationIntent::Picker),
            "only admitted navigation intents are queued"
        );
        // Config is captured only after earlier connection mutations have settled.
        let preparation = Box::new(PickerPreparation {
            config: self.config.clone(),
            target: self.app_server_target.clone(),
            cli_kv_overrides: self.cli_kv_overrides.clone(),
            loader_overrides: self.loader_overrides.clone(),
            state_db: self.state_db.clone(),
            environment_manager: Arc::clone(&self.environment_manager),
        });
        let cwd = match server.thread_params_mode() {
            crate::app_server_session::ThreadParamsMode::Embedded => {
                preparation.config.cwd.to_path_buf()
            }
            crate::app_server_session::ThreadParamsMode::Remote => server
                .remote_cwd_override()
                .unwrap_or(Path::new("."))
                .to_path_buf(),
        };
        operation.intent = NavigationIntent::PickerDefaults(preparation);
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        let completions = self.server_operations.control_tx.clone();
        let handle = server.request_handle();
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(
                crate::config_update::read_effective_config_if_supported(handle, &cwd),
            )
            .catch_unwind()
            .await
            .map_err(|_| "Picker defaults task terminated unexpectedly".to_string())
            .and_then(|result| result.map_err(|error| error.to_string()));
            let _ = completions.send(ServerCompletion::Navigation(NavigationCompletion {
                id,
                order,
                epoch,
                generation,
                receipt: NavigationReceipt::PickerDefaults(result),
            }));
        })));
        self.server_operations.navigation.current = Some(operation);
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
            // Only canceled read-only work can finish without a retained owner.
            assert!(
                !matches!(
                    &completion.receipt,
                    NavigationReceipt::Selected(
                        selection::SelectionReceipt::Resumed { .. }
                            | selection::SelectionReceipt::Detached(_)
                            | selection::SelectionReceipt::SideDetached(_)
                    )
                ),
                "resource-producing navigation receipt lost its owner"
            );
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
            NavigationReceipt::Discovered(receipt) if matching => {
                self.finish_discovery(tui, server, operation, receipt)
                    .await?;
            }
            NavigationReceipt::Discovered(_) => {}
            NavigationReceipt::Selected(receipt) => {
                self.finish_selection_operation(tui, server, operation, matching, receipt)
                    .await?;
            }
            NavigationReceipt::PickerDefaults(Ok(defaults)) if matching => {
                let NavigationIntent::PickerDefaults(mut preparation) =
                    std::mem::replace(&mut operation.intent, NavigationIntent::PickerStarting)
                else {
                    unreachable!("only the defaults stage emits a defaults receipt");
                };
                if let Some(defaults) = defaults {
                    crate::projectless::apply_defaults(
                        &mut preparation.config,
                        &self.harness_overrides,
                        server,
                        &self.environment_manager,
                        &defaults,
                    );
                    super::new_session::overlay_new_session_defaults(
                        &mut preparation.config,
                        &defaults.config,
                        &preparation.cli_kv_overrides,
                        &self.harness_overrides,
                    );
                }
                self.spawn_picker_preparation(&mut operation, *preparation);
                self.server_operations.navigation.current = Some(operation);
            }
            NavigationReceipt::PickerDefaults(Err(error)) if matching => {
                self.add_session_picker_error(format!(
                    "Failed to read session picker defaults: {error}"
                ));
                self.chat_widget.maybe_send_next_queued_input();
            }
            NavigationReceipt::PickerDefaults(_) => {}
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

    fn spawn_picker_preparation(
        &self,
        operation: &mut PendingNavigation,
        preparation: PickerPreparation,
    ) {
        let completions = self.server_operations.control_tx.clone();
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        operation.intent = NavigationIntent::PickerStarting;
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(crate::start_app_server_for_picker(
                &preparation.config,
                &preparation.target,
                preparation.cli_kv_overrides,
                preparation.loader_overrides,
                preparation.state_db,
                preparation.environment_manager,
            ))
            .catch_unwind()
            .await
            .map_err(|_| "Picker preparation task terminated unexpectedly".to_string())
            .and_then(|result| result.map_err(|error| error.to_string()));
            let _ = completions.send(ServerCompletion::Navigation(NavigationCompletion {
                id,
                order,
                epoch,
                generation,
                receipt: NavigationReceipt::Picker(result),
            }));
        })));
    }

    fn spawn_picker_cleanup(&self, operation: &mut PendingNavigation, session: AppServerSession) {
        let completions = self.server_operations.control_tx.clone();
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            match std::panic::AssertUnwindSafe(session.shutdown())
                .catch_unwind()
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "failed to close superseded picker session")
                }
                Err(_) => tracing::warn!("superseded picker cleanup task terminated unexpectedly"),
            }
            let _ = completions.send(ServerCompletion::Navigation(NavigationCompletion {
                id,
                order,
                epoch,
                generation,
                receipt: NavigationReceipt::PickerClosed,
            }));
        })));
    }
}
