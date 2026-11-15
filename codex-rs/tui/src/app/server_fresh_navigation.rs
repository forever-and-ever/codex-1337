//! Fresh acquisition retains the unsent initial draft independently of subscription custody.

use super::ACK_TIMEOUT;
use super::AbortOnDropHandle;
use super::App;
use super::AppServerSession;
use super::AppServerStartedThread;
use super::AppServerTarget;
use super::Arc;
use super::CODEX_CLI_VERSION;
use super::CleanupContinuation;
use super::Config;
use super::ConnectionMutation;
use super::DetachScope;
use super::FutureExt;
use super::LaneState;
use super::LocalDraftRecovery;
use super::NavigationCompletion;
use super::NavigationIntent;
use super::NavigationReceipt;
use super::OperationOrigin;
use super::Path;
use super::PathBuf;
use super::PendingNavigation;
use super::ProviderModelChoice;
use super::Result;
use super::ServerCompletion;
use super::ServerOperation;
use super::ThreadId;
use super::ThreadStartSource;
use super::UserMessage;
use super::apply_managed_new_thread_defaults;
use super::detach;
use super::navigation_config;
use super::tui;
use crate::app::session_lifecycle::ThreadAttachPresentation;
use crate::app_server_session::session_preparation::PreparedSession;
use crate::app_server_session::session_preparation::StartPreparation;
use crate::app_server_session::session_preparation::SubscriptionCustody;
use crate::app_server_session::session_preparation::ThreadStartChoice;
use crate::app_server_session::typed_requests::SessionRpc;
use crate::local_settings::LocalSettings;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

enum FreshMessage {
    Submit(UserMessage),
    Draft(UserMessage),
    Plan(String),
}

impl FreshMessage {
    fn into_message(self) -> UserMessage {
        match self {
            Self::Submit(message) | Self::Draft(message) => message,
            Self::Plan(plan) => plan.into(),
        }
    }
}

pub(super) struct FreshInput {
    origin: OperationOrigin,
    message: Option<FreshMessage>,
    source: Option<ThreadStartSource>,
    name: Option<String>,
    provider: Option<ProviderModelChoice>,
}

pub(super) struct FreshContext {
    input: FreshInput,
    config: Config,
    settings: LocalSettings,
}

pub(super) struct FreshReady {
    context: FreshContext,
    started: AppServerStartedThread,
    name_error: Option<String>,
}

pub(super) enum FreshStage {
    Admitted(Box<FreshInput>),
    Defaults {
        input: Box<FreshInput>,
    },
    Starting(Box<FreshContext>),
    Naming(Box<FreshReady>),
    Planning(Box<FreshReady>),
    Detaching {
        ready: Box<FreshReady>,
        plan: detach::DetachPlan,
    },
    RestoreDraft(detach::DetachPlan),
}

pub(super) struct FreshReceipt {
    handle: AppServerRequestHandle,
    outcome: FreshOutcome,
}

impl FreshReceipt {
    pub(super) fn requires_owner(&self) -> bool {
        matches!(
            self.outcome,
            FreshOutcome::Started(_)
                | FreshOutcome::Named(_)
                | FreshOutcome::Planned(_)
                | FreshOutcome::Detached(_)
        )
    }
}

enum FreshOutcome {
    Defaults(
        Result<
            Box<(
                Option<crate::config_update::EffectiveConfig>,
                Config,
                LocalSettings,
            )>,
            String,
        >,
    ),
    Started(Box<PreparedSession>),
    Named(Result<String, String>),
    Planned(Result<String, String>),
    Detached(detach::DetachReceipt),
    Terminated,
}

enum FreshEffect {
    Defaults {
        cwd: PathBuf,
        config: Box<navigation_config::ConfigRebuild>,
    },
    Start(Box<StartPreparation>),
    Name {
        thread_id: ThreadId,
        name: String,
    },
    Plan {
        thread_id: ThreadId,
        markdown: String,
        files: crate::app_server_session::fs::AppServerFileSystem,
        codex_home: Option<codex_app_server_client::AppServerPath>,
    },
    Detach(detach::DetachPlan),
}

impl FreshStage {
    pub(super) fn is_read_only(&self) -> bool {
        matches!(self, Self::Admitted(_) | Self::Defaults { .. })
    }

    pub(super) fn fences_thread(&self, id: ThreadId) -> bool {
        match self {
            Self::Naming(ready) | Self::Planning(ready) => ready.started.session.thread_id == id,
            Self::Detaching { ready, plan } => {
                ready.started.session.thread_id == id
                    || plan.targets.iter().any(|target| target.thread_id == id)
            }
            Self::RestoreDraft(plan) => plan.targets.iter().any(|target| target.thread_id == id),
            _ => false,
        }
    }

    pub(super) fn take_draft(&mut self) -> Option<LocalDraftRecovery> {
        let input = match self {
            Self::Admitted(input) | Self::Defaults { input, .. } => input,
            Self::Starting(context) => &mut context.input,
            Self::Naming(ready) | Self::Planning(ready) | Self::Detaching { ready, .. } => {
                &mut ready.context.input
            }
            Self::RestoreDraft(_) => return None,
        };
        input.message.take().map(|message| LocalDraftRecovery {
            origin: input.origin.clone(),
            message: message.into_message(),
        })
    }
}

impl App {
    pub(in crate::app) fn enqueue_fresh_navigation(
        &mut self,
        server: &AppServerSession,
        source: Option<ThreadStartSource>,
        message: Option<UserMessage>,
        name: Option<String>,
        provider: Option<ProviderModelChoice>,
    ) {
        self.enqueue_fresh_input(
            server,
            source,
            message.map(FreshMessage::Submit),
            name,
            provider,
        );
    }

    pub(in crate::app) fn enqueue_approved_plan_navigation(
        &mut self,
        server: &AppServerSession,
        plan: String,
    ) {
        self.enqueue_fresh_input(
            server,
            Some(ThreadStartSource::Clear),
            Some(FreshMessage::Plan(plan)),
            None,
            None,
        );
    }

    pub(in crate::app) fn enqueue_fresh_draft_navigation(
        &mut self,
        server: &AppServerSession,
        message: UserMessage,
    ) {
        self.enqueue_fresh_input(server, None, Some(FreshMessage::Draft(message)), None, None);
    }

    fn enqueue_fresh_input(
        &mut self,
        server: &AppServerSession,
        source: Option<ThreadStartSource>,
        message: Option<FreshMessage>,
        name: Option<String>,
        provider: Option<ProviderModelChoice>,
    ) {
        let origin =
            self.chat_widget
                .thread_id()
                .map_or(OperationOrigin::Connection, |thread_id| {
                    OperationOrigin::Thread {
                        thread_id,
                        store: Arc::clone(&self.ensure_thread_channel(thread_id).store),
                    }
                });
        self.enqueue_navigation(
            server,
            NavigationIntent::Fresh(FreshStage::Admitted(Box::new(FreshInput {
                origin,
                message,
                source,
                name,
                provider,
            }))),
        );
    }

    pub(in crate::app) async fn enqueue_parked_draft_restore(&mut self, server: &AppServerSession) {
        let plan = self
            .prepare_detach_plan(DetachScope::Navigation { destination: None })
            .await;
        self.enqueue_navigation(
            server,
            NavigationIntent::Fresh(FreshStage::RestoreDraft(plan)),
        );
    }

    pub(super) fn dispatch_fresh_defaults(
        &self,
        server: &AppServerSession,
        operation: &mut PendingNavigation,
    ) {
        if let NavigationIntent::Fresh(FreshStage::RestoreDraft(plan)) = &operation.intent {
            let plan = plan.clone();
            self.spawn_fresh_effect(
                operation,
                server.request_handle(),
                FreshEffect::Detach(plan),
            );
            return;
        }
        let NavigationIntent::Fresh(FreshStage::Admitted(input)) =
            std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        else {
            unreachable!("fresh defaults require admitted input");
        };
        let cwd = self.chat_widget.config_ref().cwd.to_path_buf();
        let defaults_cwd = match server.thread_params_mode() {
            crate::app_server_session::ThreadParamsMode::Embedded => cwd.clone(),
            crate::app_server_session::ThreadParamsMode::Remote => server
                .remote_cwd_override()
                .unwrap_or(Path::new("."))
                .to_path_buf(),
        };
        operation.intent = NavigationIntent::Fresh(FreshStage::Defaults { input });
        self.spawn_fresh_effect(
            operation,
            server.request_handle(),
            FreshEffect::Defaults {
                cwd: defaults_cwd,
                config: self.capture_config_rebuild(cwd.clone(), Some(&cwd)),
            },
        );
    }

    fn spawn_fresh_effect(
        &self,
        operation: &mut PendingNavigation,
        handle: AppServerRequestHandle,
        effect: FreshEffect,
    ) {
        let completions = self.server_operations.control_tx.clone();
        let (id, order, epoch, generation) = (
            operation.id,
            operation.order,
            operation.epoch,
            operation.generation,
        );
        operation.state = LaneState::Running(AbortOnDropHandle::new(tokio::spawn(async move {
            let outcome = Box::pin(
                std::panic::AssertUnwindSafe(async {
                    match effect {
                        FreshEffect::Defaults { cwd, config } => {
                            let defaults = tokio::time::timeout(
                                ACK_TIMEOUT,
                                crate::config_update::read_effective_config_if_supported(
                                    handle.clone(),
                                    &cwd,
                                ),
                            )
                            .await
                            .map_err(|_| "New-session defaults read timed out".to_string())
                            .and_then(|result| result.map_err(|error| error.to_string()));
                            let result = match defaults {
                                Ok(defaults) => config
                                    .run()
                                    .await
                                    .map(|projected| {
                                        let (config, settings) = *projected;
                                        Box::new((defaults, config, settings))
                                    })
                                    .map_err(|error| error.to_string()),
                                Err(error) => Err(error),
                            };
                            FreshOutcome::Defaults(result)
                        }
                        FreshEffect::Start(plan) => {
                            FreshOutcome::Started(Box::new(Box::pin(plan.run()).await))
                        }
                        FreshEffect::Plan {
                            thread_id,
                            markdown,
                            files,
                            codex_home,
                        } => {
                            let attachment_path = crate::attachment_files::write_approved_plan(
                                &files,
                                codex_home.as_ref(),
                                markdown.clone(),
                            )
                            .await
                            .inspect_err(
                                |err| tracing::debug!(%err, "approved plan attachment unavailable"),
                            )
                            .ok()
                            .map(|path| path.to_string());
                            let result = handle
                                .thread_prepare_approved_plan(
                                    api::RequestId::String(format!("tui-operation-{id}-plan")),
                                    api::ThreadPrepareApprovedPlanParams {
                                        thread_id: thread_id.to_string(),
                                        markdown,
                                        attachment_path,
                                    },
                                )
                                .await
                                .map(|response| response.prompt)
                                .map_err(|error| error.to_string());
                            FreshOutcome::Planned(result)
                        }
                        FreshEffect::Name { thread_id, name } => {
                            let result = tokio::time::timeout(
                                ACK_TIMEOUT,
                                handle.thread_allocate_name(
                                    api::RequestId::String(format!("tui-operation-{id}-name")),
                                    api::ThreadSetNameParams {
                                        thread_id: thread_id.to_string(),
                                        name: name.clone(),
                                        naming_mode: Some(api::ThreadNameMode::Explicit),
                                        conflict_thread_ids: None,
                                    },
                                ),
                            )
                            .await
                            .map_err(|_| {
                                "Name acknowledgement timed out; the name write may still complete"
                                    .to_string()
                            })
                            .and_then(|result| {
                                result
                                    .map(|response| response.thread_name.unwrap_or(name))
                                    .map_err(|error| error.to_string())
                            });
                            FreshOutcome::Named(result)
                        }
                        FreshEffect::Detach(plan) => FreshOutcome::Detached(
                            match tokio::time::timeout(
                                ACK_TIMEOUT,
                                detach::send(&handle, id, &plan),
                            )
                            .await
                            {
                                Ok(results) => detach::DetachReceipt::Complete(results),
                                Err(_) => detach::DetachReceipt::BudgetExpired,
                            },
                        ),
                    }
                })
                .catch_unwind(),
            )
            .await
            .unwrap_or(FreshOutcome::Terminated);
            let _ = completions.send(ServerCompletion::Navigation(Box::new(
                NavigationCompletion {
                    id,
                    order,
                    epoch,
                    generation,
                    receipt: NavigationReceipt::Fresh(FreshReceipt { handle, outcome }),
                },
            )));
        })));
    }

    fn recover_fresh_input(&mut self, input: &mut FreshInput) {
        if let Some(message) = input.message.take() {
            self.queue_local_draft_recovery(LocalDraftRecovery {
                origin: input.origin.clone(),
                message: message.into_message(),
            });
        }
    }

    async fn detach_for_fresh_commit(
        &mut self,
        server: &AppServerSession,
        mut operation: PendingNavigation,
        ready: Box<FreshReady>,
    ) {
        let plan = self
            .prepare_detach_plan(DetachScope::Navigation {
                destination: Some(ready.started.session.thread_id),
            })
            .await;
        operation.intent = NavigationIntent::Fresh(FreshStage::Detaching {
            ready,
            plan: plan.clone(),
        });
        self.spawn_fresh_effect(
            &mut operation,
            server.request_handle(),
            FreshEffect::Detach(plan),
        );
        self.server_operations.navigation.current = Some(operation);
    }

    pub(super) async fn finish_fresh_navigation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        mut operation: PendingNavigation,
        matching: bool,
        receipt: FreshReceipt,
    ) -> Result<()> {
        let same_epoch = operation.epoch == self.server_operations.epoch;
        let NavigationIntent::Fresh(stage) =
            std::mem::replace(&mut operation.intent, NavigationIntent::Transitioning)
        else {
            unreachable!("fresh receipt without fresh operation");
        };
        let FreshReceipt { handle, outcome } = receipt;
        match (stage, outcome) {
            (FreshStage::RestoreDraft(plan), FreshOutcome::Detached(receipt)) => {
                let fences = match &receipt {
                    detach::DetachReceipt::Complete(results) => results
                        .iter()
                        .filter(|result| result.result.is_err())
                        .map(|result| result.thread_id)
                        .collect::<Vec<_>>(),
                    detach::DetachReceipt::BudgetExpired => {
                        plan.targets.iter().map(|target| target.thread_id).collect()
                    }
                };
                if same_epoch && let detach::DetachReceipt::Complete(results) = receipt {
                    self.finish_detach_plan(
                        &plan,
                        detach::DetachReceipt::Complete(
                            results
                                .into_iter()
                                .filter(|result| result.result.is_ok())
                                .collect(),
                        ),
                    )
                    .await;
                }
                if same_epoch && !fences.is_empty() {
                    self.chat_widget.add_error_message("Draft restoration detachment was not confirmed. Close and reopen this TUI to resynchronize; no detach was retried.".into());
                    self.retain_navigation_uncertainty(operation, fences, false);
                } else if matching {
                    self.commit_parked_session_draft(tui, server).await?;
                }
            }
            (FreshStage::Defaults { mut input }, FreshOutcome::Defaults(result)) => {
                if !matching {
                    self.recover_fresh_input(&mut input);
                    return Ok(());
                }
                let result = match result {
                    Ok(projected) => {
                        let (defaults, config, settings) = *projected;
                        self.project_new_session_config(server, (config, settings), defaults)
                    }
                    Err(error) => Err(color_eyre::eyre::eyre!(error)),
                };
                let (mut config, settings) = match result {
                    Ok(result) => result,
                    Err(error) => {
                        self.recover_fresh_input(&mut input);
                        self.chat_widget.add_error_message(format!(
                            "Failed to read new session defaults: {error}"
                        ));
                        return Ok(());
                    }
                };
                apply_managed_new_thread_defaults(
                    &mut config,
                    server.managed_new_thread_defaults(),
                    &self.cli_kv_overrides,
                    &self.harness_overrides,
                );
                let choice = if let Some(choice) = &input.provider {
                    config.model_provider_id.clone_from(&choice.provider.id);
                    if let Some(info) = config.model_providers.get(&choice.provider.id) {
                        config.model_provider = info.clone();
                    }
                    config.model = Some(choice.model.model.clone());
                    config.model_reasoning_effort = Some(choice.effort.clone());
                    config.service_tier = Some("default".into());
                    ThreadStartChoice::Provider(choice.provider.id.clone())
                } else {
                    ThreadStartChoice::Configured(input.source)
                };
                let plan = server.prepare_start_thread(&self.local_settings, &config, choice);
                operation.intent =
                    NavigationIntent::Fresh(FreshStage::Starting(Box::new(FreshContext {
                        input: *input,
                        config,
                        settings,
                    })));
                self.spawn_fresh_effect(&mut operation, handle, FreshEffect::Start(Box::new(plan)));
                self.server_operations.navigation.current = Some(operation);
            }
            (FreshStage::Starting(mut context), FreshOutcome::Started(prepared)) => {
                let PreparedSession {
                    result,
                    custody,
                    observations,
                } = *prepared;
                let valid_origin = server.accepts_session_observations(&observations);
                let matching = matching && valid_origin;
                let result = if valid_origin {
                    result
                } else {
                    Err(color_eyre::eyre::eyre!(
                        "prepared session belongs to a different backend"
                    ))
                };
                if same_epoch && valid_origin {
                    server.apply_session_observations(observations)?;
                }
                let started = match result {
                    Ok(started) if matching => started,
                    result => {
                        self.recover_fresh_input(&mut context.input);
                        if matching && let Err(error) = &result {
                            self.chat_widget.add_error_message(format!(
                                "Failed to start a fresh session through the app server: {error}"
                            ));
                        }
                        match custody {
                            SubscriptionCustody::Subscribed(thread_id) => self
                                .start_navigation_cleanup(
                                    operation,
                                    handle,
                                    thread_id,
                                    Vec::new(),
                                    CleanupContinuation::Done,
                                    false,
                                ),
                            SubscriptionCustody::NewThreadUncertain
                            | SubscriptionCustody::Uncertain(_)
                                if same_epoch =>
                            {
                                self.chat_widget.add_error_message("New conversation acquisition outcome is unknown. No start was retried. Close and reopen this TUI to resynchronize; the earlier request may still complete.".into());
                                self.retain_navigation_uncertainty(operation, Vec::new(), false);
                            }
                            _ => {}
                        }
                        return Ok(());
                    }
                };
                let ready = Box::new(FreshReady {
                    context: *context,
                    started,
                    name_error: None,
                });
                if let Some(FreshMessage::Plan(markdown)) = &ready.context.input.message {
                    let effect = FreshEffect::Plan {
                        thread_id: ready.started.session.thread_id,
                        markdown: markdown.clone(),
                        files: server.file_system(),
                        codex_home: server.codex_home_path(&ready.context.config.codex_home),
                    };
                    operation.intent = NavigationIntent::Fresh(FreshStage::Planning(ready));
                    self.spawn_fresh_effect(&mut operation, handle, effect);
                    self.server_operations.navigation.current = Some(operation);
                } else if let Some(name) = ready.context.input.name.clone() {
                    let thread_id = ready.started.session.thread_id;
                    operation.intent = NavigationIntent::Fresh(FreshStage::Naming(ready));
                    self.spawn_fresh_effect(
                        &mut operation,
                        handle,
                        FreshEffect::Name { thread_id, name },
                    );
                    self.server_operations.navigation.current = Some(operation);
                } else {
                    self.detach_for_fresh_commit(server, operation, ready).await;
                }
            }
            (FreshStage::Planning(mut ready), FreshOutcome::Planned(result)) => match result {
                Ok(prompt) if matching => {
                    ready.context.input.message = Some(FreshMessage::Submit(prompt.into()));
                    self.detach_for_fresh_commit(server, operation, ready).await;
                }
                result => {
                    self.recover_fresh_input(&mut ready.context.input);
                    if matching && let Err(error) = result {
                        self.chat_widget.add_error_message(format!(
                            "Failed to hand off the approved plan: {error}"
                        ));
                    }
                    self.start_navigation_cleanup(
                        operation,
                        handle,
                        ready.started.session.thread_id,
                        Vec::new(),
                        CleanupContinuation::Done,
                        false,
                    );
                }
            },
            (FreshStage::Naming(mut ready), FreshOutcome::Named(result)) => {
                if !matching {
                    self.recover_fresh_input(&mut ready.context.input);
                    self.start_navigation_cleanup(
                        operation,
                        handle,
                        ready.started.session.thread_id,
                        Vec::new(),
                        CleanupContinuation::Done,
                        false,
                    );
                    return Ok(());
                }
                match result {
                    Ok(name) => ready.started.session.thread_name = Some(name),
                    Err(error) => {
                        ready.name_error = Some(format!(
                            "Failed to name the new session: {error}. No name write was retried."
                        ));
                    }
                }
                self.detach_for_fresh_commit(server, operation, ready).await;
            }
            (FreshStage::Detaching { mut ready, plan }, FreshOutcome::Detached(receipt)) => {
                let fences = match &receipt {
                    detach::DetachReceipt::Complete(results) => results
                        .iter()
                        .filter(|result| result.result.is_err())
                        .map(|result| result.thread_id)
                        .collect::<Vec<_>>(),
                    detach::DetachReceipt::BudgetExpired => {
                        plan.targets.iter().map(|target| target.thread_id).collect()
                    }
                };
                if same_epoch && let detach::DetachReceipt::Complete(results) = receipt {
                    // An unconfirmed unsubscribe is not authority to retire its local listener.
                    let confirmed = results
                        .into_iter()
                        .filter(|result| result.result.is_ok())
                        .collect();
                    self.finish_detach_plan(&plan, detach::DetachReceipt::Complete(confirmed))
                        .await;
                }
                if !matching || !fences.is_empty() {
                    self.recover_fresh_input(&mut ready.context.input);
                    if same_epoch && !fences.is_empty() {
                        self.chat_widget.add_error_message("Navigation detachment was not confirmed. The old conversation remains selected; close and reopen this TUI to resynchronize. No detach was retried.".into());
                    }
                    self.start_navigation_cleanup(
                        operation,
                        handle,
                        ready.started.session.thread_id,
                        if same_epoch { fences } else { Vec::new() },
                        CleanupContinuation::Done,
                        false,
                    );
                } else {
                    Box::pin(self.commit_fresh_navigation(tui, server, operation, *ready)).await?;
                }
            }
            (mut stage, FreshOutcome::Terminated) => {
                if let Some(draft) = stage.take_draft() {
                    self.queue_local_draft_recovery(draft);
                }
                self.chat_widget.add_error_message("New conversation preparation terminated unexpectedly. No request was retried; close and reopen this TUI to resynchronize.".into());
                match stage {
                    FreshStage::RestoreDraft(plan) if same_epoch => self
                        .retain_navigation_uncertainty(
                            operation,
                            plan.targets.iter().map(|target| target.thread_id).collect(),
                            false,
                        ),
                    FreshStage::Starting(_) if same_epoch => {
                        self.retain_navigation_uncertainty(operation, Vec::new(), false);
                    }
                    FreshStage::Naming(ready) | FreshStage::Planning(ready) => self
                        .start_navigation_cleanup(
                            operation,
                            handle,
                            ready.started.session.thread_id,
                            Vec::new(),
                            CleanupContinuation::Done,
                            false,
                        ),
                    FreshStage::Detaching { ready, plan } => self.start_navigation_cleanup(
                        operation,
                        handle,
                        ready.started.session.thread_id,
                        if same_epoch {
                            plan.targets.iter().map(|target| target.thread_id).collect()
                        } else {
                            Vec::new()
                        },
                        CleanupContinuation::Done,
                        false,
                    ),
                    _ => {}
                }
            }
            _ => unreachable!("fresh stage received a different stage's receipt"),
        }
        Ok(())
    }

    async fn commit_fresh_navigation(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        operation: PendingNavigation,
        mut ready: FreshReady,
    ) -> Result<()> {
        if let Some(thread_id) = self.current_displayed_thread_id()
            && let Some(blank) = self.agents_overview.blank_sessions.get_mut(&thread_id)
        {
            if let Some(channel) = self.thread_event_channels.get(&thread_id)
                && let Some(session) = channel.store.lock().await.session.as_ref()
            {
                blank.session = session.clone();
            }
            if let Some(input) = self.chat_widget.capture_thread_input_state() {
                self.agents_overview.input_states.insert(thread_id, input);
            }
        }
        if let Some(choice) = &ready.context.input.provider {
            let catalog = Arc::make_mut(&mut self.model_catalog);
            catalog.models = vec![choice.model.clone()];
            server.set_available_models(catalog.models.clone());
        }
        self.local_settings = ready.context.settings;
        self.config = ready.context.config;
        self.refresh_server_version_overview_notice(CODEX_CLI_VERSION);
        self.remember_launch_permissions();
        let thread_id = ready.started.session.thread_id;
        if !self.config.ephemeral && !matches!(self.app_server_target, AppServerTarget::Embedded) {
            self.agents_overview
                .blank_sessions
                .insert(thread_id, ready.started.clone());
        }
        // Keep the move-only draft out of the fallible renderer. Admit it only after installation.
        if let Err(error) = Box::pin(self.replace_chat_widget_with_app_server_thread(
            tui,
            ready.started,
            ThreadAttachPresentation::Fresh,
            None,
        ))
        .await
        {
            self.agents_overview.blank_sessions.remove(&thread_id);
            self.recover_fresh_input(&mut ready.context.input);
            self.chat_widget.add_error_message(format!(
                "Failed to attach to fresh app-server thread: {error}"
            ));
            if self.chat_widget.thread_id() != Some(thread_id) {
                self.start_navigation_cleanup(
                    operation,
                    server.request_handle(),
                    thread_id,
                    Vec::new(),
                    CleanupContinuation::Done,
                    false,
                );
            }
            return Ok(());
        }
        match ready.context.input.message.take() {
            Some(FreshMessage::Submit(message)) => {
                self.chat_widget.initial_user_message = Some(message);
                self.chat_widget.submit_initial_user_message_if_pending();
            }
            Some(FreshMessage::Plan(_)) => {
                unreachable!("approved plan preparation precedes installation")
            }
            Some(FreshMessage::Draft(message)) => {
                self.chat_widget.restore_user_message_to_composer(message);
            }
            None => {}
        }
        if let Some(choice) = ready.context.input.provider {
            self.enqueue_connection_operation(
                server,
                ServerOperation::Connection(ConnectionMutation::ModelDefaults {
                    edits: crate::config_update::build_model_selection_edits(
                        &choice.provider.id,
                        &choice.model.model,
                        Some(choice.effort),
                    ),
                    setting: "default provider, model, and reasoning effort".into(),
                }),
            );
        }
        if let Some(error) = ready.name_error {
            self.chat_widget.add_error_message(error);
        }
        tui.frame_requester().schedule_frame();
        Ok(())
    }
}
