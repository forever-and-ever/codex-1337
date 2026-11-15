//! Startup bootstrap, draft handoff, and protected-input orchestration for the TUI app.
//!
//! Owns the main app run loop from app-server bootstrap through terminal shutdown. Startup input
//! remains isolated from protected interactive requests until the initialized composer owns it.
//! Queued resume history replaces the provisional loading frame only when it is ready to render.
//! Explicit local launch permissions remain runtime overrides across new sessions and reconnects.

use super::reconnect::ReconnectState;
use super::startup_bootstrap::bootstrap_server_owned_start;
use super::startup_bootstrap::uses_server_owned_fresh_bootstrap;
use super::*;
use crate::session_start::SessionStartAction;
use crate::session_start::SessionStartConfig;
use crate::session_start::SessionStartOutcome;
use crate::session_start::cancel_session_start;
use crate::session_start::complete_session_start;
use crate::unarchive_prompt::run_unarchive_prompt;
use color_eyre::eyre::ContextCompat;

#[path = "ui_schedule.rs"]
pub(super) mod ui_schedule;

pub(super) fn spawn_startup_thread_start(
    app_server: &AppServerSession,
    local_settings: crate::local_settings::LocalSettings,
    config: Config,
    launch_choices: crate::app_server_session::StartupLaunchChoices,
    app_event_tx: AppEventSender,
    model_provider_override: Option<String>,
) {
    let backend_id = app_server.backend_id();
    let request_handle = app_server.request_handle();
    let thread_params_mode = app_server.thread_params_mode();
    let remote_cwd_override = app_server.remote_cwd_override().map(Path::to_path_buf);
    tokio::spawn(async move {
        let result = crate::app_server_session::start_thread_with_request_handle(
            request_handle.clone(),
            &local_settings,
            config,
            thread_params_mode,
            remote_cwd_override,
            model_provider_override,
            launch_choices,
        )
        .await;
        app_event_tx.send(AppEvent::StartupThreadStarted {
            backend_id,
            request_handle,
            result,
        });
    });
}

pub(super) async fn prepare_fresh_startup_config(
    config: &mut Config,
    app_server: &AppServerSession,
    app_server_target: &AppServerTarget,
    cli_kv_overrides: &[(String, TomlValue)],
    harness_overrides: &ConfigOverrides,
    environments: &EnvironmentManager,
) -> Result<crate::projectless::AppliedDefaults> {
    let defaults_cwd = match app_server_target {
        AppServerTarget::Embedded | AppServerTarget::LocalDaemon { .. } => config.cwd.as_path(),
        AppServerTarget::Remote { .. } => {
            app_server.remote_cwd_override().unwrap_or(Path::new("."))
        }
    };
    let defaults = crate::config_update::read_effective_config_with_layers(
        app_server.request_handle(),
        defaults_cwd,
    )
    .await?;
    super::new_session::overlay_new_session_defaults(
        config,
        &defaults.config,
        cli_kv_overrides,
        harness_overrides,
    );
    let projectless = crate::projectless::apply_defaults(
        config,
        harness_overrides,
        app_server,
        environments,
        &defaults,
    );
    apply_managed_new_thread_defaults(
        config,
        app_server.managed_new_thread_defaults(),
        cli_kv_overrides,
        harness_overrides,
    );
    Ok(projectless)
}

pub(super) fn startup_model(
    config: &Config,
    bootstrap: &AppServerBootstrap,
    server_defaults_read: bool,
) -> Result<String> {
    config
        .model
        .clone()
        .or_else(|| {
            if server_defaults_read {
                // Legacy bootstrap paths may be seeded with local config, which can differ from a
                // cleared server model. Use the server catalog when config/read returned model: null.
                bootstrap
                    .available_models
                    .iter()
                    .find(|model| model.is_default)
                    .or_else(|| bootstrap.available_models.first())
                    .map(|model| model.model.clone())
                    .or_else(|| bootstrap.default_model.clone())
            } else {
                bootstrap.default_model.clone()
            }
        })
        .wrap_err("The initial app-server connection did not provide a startup model.")
}

impl App {
    /// Keep explicit local launch choices available for new sessions and reconnect recovery.
    pub(super) fn remember_launch_permissions(&mut self) {
        if self.app_server_target.thread_params_mode()
            == crate::app_server_session::ThreadParamsMode::Remote
        {
            return;
        }
        let selected = crate::resume_permissions::ResumePermissions::from_overrides(
            &self.config,
            &self.harness_overrides,
        );
        if selected.approval_policy {
            self.runtime_approval_policy_override = Some(RuntimeApprovalPolicyOverride::Explicit(
                self.config.permissions.approval_policy.value().into(),
            ));
        }
        if selected.approvals_reviewer {
            self.runtime_approvals_reviewer_override = Some(self.config.approvals_reviewer);
        }
        if selected.profile {
            let profile = RuntimePermissionProfileOverride::from_config(&self.config);
            // The server owns constrained profiles that legacy turn parameters cannot express.
            self.runtime_permission_profile_override =
                (profile.active_permission_profile.is_some()
                    || crate::app_server_session::turn_permissions_overrides(
                        TurnPermissionsOverride::LegacySandbox(
                            self.config.permissions.effective_permission_profile(),
                        ),
                        self.config.cwd.as_path(),
                    )
                    .is_ok())
                .then_some(profile);
        }
    }

    pub(super) fn awaiting_side_replay(&self, tui: &tui::Tui) -> bool {
        self.pending_thread_switch_resets != 0 && self.side_retention_enabled(tui)
    }

    /// Keep the provisional loading frame until queued history reaches the owned transcript.
    /// Visible startup decisions and the agent overview must still render immediately.
    pub(super) fn render_startup_frame(
        &mut self,
        tui: &mut tui::Tui,
        app_event_rx: &mpsc::UnboundedReceiver<AppEvent>,
    ) -> Result<()> {
        if self.awaiting_side_replay(tui)
            || (tui.is_owned_screen()
                && !app_event_rx.is_empty()
                && !self.chat_widget.has_active_view())
        {
            return Ok(());
        }
        self.chat_widget.pre_draw_tick();
        if tui.presentation_mode() == crate::tui::PresentationMode::Frozen {
            return Ok(());
        }
        self.render_chat_widget_frame(tui, tui.terminal.last_known_screen_size)?;
        if self.chat_widget.has_active_modal() && self.startup_protected_input_boundary {
            self.discard_startup_modal_input(tui)?;
        }
        Ok(())
    }

    /// Keep a visible modal closed to input if the terminal has not settled yet. The next input
    /// event retries the drain instead of acting on the modal; server events can continue meanwhile.
    pub(super) fn discard_startup_modal_input(&mut self, tui: &mut tui::Tui) -> Result<()> {
        match tui.discard_pending_input_before_interactive_screen() {
            Ok(()) => self.startup_pending_protected_request = false,
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {
                self.startup_pending_protected_request = true;
                tracing::warn!(%err, "keeping startup modal input quarantined");
            }
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }

    /// Recognizes queued requests before they become visible protected screens.
    pub(super) fn has_queued_startup_protected_request(&self) -> bool {
        self.startup_protected_input_boundary
            && (self
                .active_thread_rx
                .as_ref()
                .is_some_and(|receiver| !receiver.is_empty())
                && self
                    .active_thread_id
                    .and_then(|thread_id| self.thread_event_channels.get(&thread_id))
                    .is_none_or(|channel| {
                        // A bounded drain can leave ordinary notifications queued. Only protect
                        // input for pending requests, or when their state cannot be inspected.
                        channel.store.try_lock().map_or(/*default*/ true, |store| {
                            store.side_parent_pending_status().is_some()
                        })
                    })
                || self
                    .pending_primary_events
                    .iter()
                    .any(|event| matches!(event, ThreadBufferedEvent::Request(_))))
    }

    /// Wait until visible and queued startup decisions cannot consume a delayed OSC response.
    #[cfg(any(windows, test))]
    pub(super) fn ready_for_terminal_color_probe(&self, has_pending_app_events: bool) -> bool {
        !has_pending_app_events
            && !self.chat_widget.has_active_view()
            && !self.startup_pending_protected_request
            && !self.has_queued_startup_protected_request()
            && !self.chat_widget.has_pending_protected_request()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        mut app_server: AppServerSession,
        mut config: Config,
        launch_cwd: PathBuf,
        local_shell: super::local_shell::LocalShell,
        cli_kv_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
        loader_overrides: LoaderOverrides,
        cloud_config_bundle: CloudConfigBundleLoader,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        session_selection: SessionSelection,
        is_first_run: bool,
        should_prompt_windows_sandbox_nux_at_startup: bool,
        app_server_target: AppServerTarget,
        environment_manager: Arc<EnvironmentManager>,
        startup_elapsed_before_app: Duration,
        startup_bootstrap: Option<AppServerBootstrap>,
        startup_hooks_browser: Option<HooksListEntry>,
        daemon_startup_warning: Option<String>,
        managed_daemon: bool,
        mut startup_draft: StartupDraftPump,
    ) -> Result<AppExitInfo> {
        use crate::ui_latency::Stage;
        use crate::ui_latency::enter;
        let startup_stage = enter(Stage::Startup);

        async fn shutdown_startup_sessions(sessions: impl IntoIterator<Item = AppServerSession>) {
            let shutdowns = sessions.into_iter().map(AppServerSession::shutdown);
            let shutdown_result: std::io::Result<Vec<()>> = futures::future::join_all(shutdowns)
                .await
                .into_iter()
                .collect();
            if let Err(shutdown_error) = shutdown_result {
                tracing::warn!("app-server shutdown failed: {shutdown_error}");
            }
        }

        async fn shutdown_on_startup_error(
            app_server: AppServerSession,
            error: impl Into<color_eyre::eyre::Report>,
        ) -> Result<AppExitInfo> {
            shutdown_startup_sessions([app_server]).await;
            Err(error.into())
        }

        async fn cancel_startup_sessions(app_server: AppServerSession) -> AppExitInfo {
            let exit_info = cancel_session_start(app_server).await;
            exit_info
        }

        // Every startup failure releases the captured backend connection.
        macro_rules! startup_try {
            ($result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(error) => {
                        return shutdown_on_startup_error(app_server, error).await;
                    }
                }
            };
        }
        // Adopt actual launch ownership before constructing session-local preferences.
        startup_try!(tui.prepare_owned_screen(config.tui_fullscreen_transcript));
        let mut local_settings = crate::local_settings::LocalSettings::for_tui(&config, tui);
        let startup_started_at = Instant::now();
        if let SessionSelection::Resume(target) | SessionSelection::Fork(target) =
            &session_selection
        {
            startup_try!(target.ensure_backend(app_server.backend_id()));
        }
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);
        if let Some(message) = project_config_warning(&config) {
            app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::StartupWarningsCell::new(vec![message]),
            )));
        }
        emit_system_bwrap_warning(&app_event_tx, &config);
        tui.set_notification_settings(&local_settings.tui.notification_settings);

        let harness_overrides = startup_try!(normalize_harness_overrides_for_cwd(
            harness_overrides,
            &config.cwd
        ));
        app_server.model_provider_override = harness_overrides.model_provider.clone();
        let fresh_start = matches!(
            &session_selection,
            SessionSelection::StartFresh | SessionSelection::Exit
        );
        let server_owned_fresh_bootstrap =
            uses_server_owned_fresh_bootstrap(&session_selection, &loader_overrides);
        let mut bootstrap = startup_bootstrap;
        if bootstrap.is_none() && !server_owned_fresh_bootstrap {
            bootstrap = Some(
                match startup_draft
                    .run_until(
                        tui,
                        app_server.bootstrap_for_selection(&config, &session_selection),
                    )
                    .await
                {
                    Ok(bootstrap) => startup_try!(bootstrap),
                    Err(err) => {
                        return shutdown_on_startup_error(app_server, err).await;
                    }
                },
            );
        }
        if matches!(&session_selection, SessionSelection::Fork(_)) {
            // The app server resolves omitted overrides from the fork destination's config.
            if harness_overrides.model.is_none()
                && !super::new_session::has_launch_setting(&config, &cli_kv_overrides, "model")
                && !super::new_session::has_launch_setting(
                    &config,
                    &cli_kv_overrides,
                    "model_reasoning_effort",
                )
            {
                config.model = None;
                config.model_reasoning_effort = None;
            }
        }
        let startup_defaults = if fresh_start {
            match startup_draft
                .run_until(
                    tui,
                    prepare_fresh_startup_config(
                        &mut config,
                        &app_server,
                        &app_server_target,
                        &cli_kv_overrides,
                        &harness_overrides,
                        &environment_manager,
                    ),
                )
                .await
            {
                Ok(Ok(defaults)) => defaults,
                Ok(Err(err)) => {
                    return shutdown_on_startup_error(app_server, err).await;
                }
                Err(err) => {
                    return shutdown_on_startup_error(app_server, err).await;
                }
            }
        } else {
            crate::projectless::AppliedDefaults::default()
        };
        let mut launch_choices = crate::app_server_session::StartupLaunchChoices::from_launch(
            &cli_kv_overrides,
            &harness_overrides,
            &loader_overrides,
        );
        let bootstrap = match bootstrap {
            Some(bootstrap) => bootstrap,
            None => match startup_draft
                .run_until(
                    tui,
                    bootstrap_server_owned_start(
                        &mut app_server,
                        &mut config,
                        &mut launch_choices,
                        &cli_kv_overrides,
                        &harness_overrides,
                    ),
                )
                .await
            {
                Ok(bootstrap) => startup_try!(bootstrap),
                Err(err) => {
                    return shutdown_on_startup_error(app_server, err).await;
                }
            },
        };
        launch_choices.apply_projectless_approval_policy(
            startup_defaults.approval_policy,
            bootstrap.allowed_approval_policies.as_deref(),
        );
        tracing::debug!(
            has_platform_family = app_server.app_server_platform_family().is_some(),
            has_platform_os = app_server.app_server_platform_os().is_some(),
            "connected app-server platform"
        );
        let bootstrap_ms = bootstrap.duration.as_millis();
        if !server_owned_fresh_bootstrap
            && matches!(&session_selection, SessionSelection::AgentsOverview)
        {
            apply_managed_new_thread_defaults(
                &mut config,
                app_server.managed_new_thread_defaults(),
                &cli_kv_overrides,
                &harness_overrides,
            );
        }
        let mut model = if fresh_start {
            Some(startup_try!(startup_model(&config, &bootstrap, true)))
        } else {
            config
                .model
                .clone()
                .or_else(|| bootstrap.default_model.clone())
        };
        let available_models = bootstrap.available_models;
        let mut remote_connection =
            crate::status::remote_connection::remote_connection_status_value(
                &app_server_target,
                app_server.server_version(),
            );
        if let Some(connection) = &mut remote_connection {
            connection.is_managed_daemon = connection.is_local_daemon && managed_daemon;
        }
        let initial_server_version_notice =
            if !matches!(app_server_target, AppServerTarget::Embedded) {
                crate::status::remote_connection::pending_server_version_notice(
                    &local_settings.tui,
                    &app_server_target,
                    app_server.server_codex_home(),
                    CODEX_CLI_VERSION,
                    app_server.server_version(),
                    /*last_shown*/ None,
                )
            } else {
                None
            };
        if let Err(err) = startup_draft.flush_pending_events(tui).await {
            return shutdown_on_startup_error(app_server, err).await;
        }
        let exit_info = if fresh_start {
            let result = handle_model_migration_prompt_if_needed(
                tui,
                &mut config,
                &local_settings,
                &mut launch_choices,
                model.as_deref(),
                &app_event_tx,
                &available_models,
            )
            .await;
            startup_try!(result)
        } else {
            None
        };
        if let Some(exit_info) = exit_info {
            shutdown_startup_sessions([app_server]).await;
            return Ok(exit_info);
        }
        if let Some(updated_model) = config.model.clone() {
            model = Some(updated_model);
        }
        let mut provider_writes = HashMap::new();
        let model_catalog = Arc::new(
            ModelCatalog::new(available_models.clone())
                .for_backend(app_server.backend_id(), &mut provider_writes)
                .with_collaboration_modes(bootstrap.collaboration_modes),
        );
        let account = bootstrap.account.clone();
        let (account_email, auth_mode) = match &account {
            crate::app_server_session::BootstrapAccount::AwaitingResume => (None, None),
            crate::app_server_session::BootstrapAccount::Loaded(account) => {
                (account.email().map(str::to_owned), account.auth_mode())
            }
        };
        let session_telemetry = SessionTelemetry::new(
            ThreadId::new(),
            model.as_deref().unwrap_or_default(),
            model.as_deref().unwrap_or_default(),
            /*account_id*/ None,
            account_email,
            auth_mode,
            codex_login::default_client::originator().value,
            config.otel.log_user_prompt,
            user_agent(),
            serde_json::from_value(serde_json::json!("cli"))
                .unwrap_or_else(|err| panic!("cli session source should deserialize: {err}")),
        );
        if local_settings
            .tui
            .status_line
            .as_ref()
            .is_some_and(|cmd| !cmd.is_empty())
        {
            session_telemetry.counter("codex.status_line", /*inc*/ 1, &[]);
        }

        let status_line_invalid_items_warned = Arc::new(AtomicBool::new(false));
        let terminal_title_invalid_items_warned = Arc::new(AtomicBool::new(false));
        let workspace_command_runner: WorkspaceCommandRunner = Arc::new(
            AppServerWorkspaceCommandRunner::new(app_server.request_handle()),
        );
        let enhanced_keys_supported = tui.enhanced_keys_supported();
        let draft_session = matches!(&session_selection, SessionSelection::StartFresh)
            && initial_prompt.is_none()
            && initial_images.is_empty()
            && harness_overrides.model.is_none()
            && harness_overrides.model_provider.is_none()
            && !super::new_session::has_launch_setting(&config, &cli_kv_overrides, "model")
            && !super::new_session::has_launch_setting(
                &config,
                &cli_kv_overrides,
                "model_provider",
            );
        #[cfg(target_os = "linux")]
        let draft_session = draft_session || crate::reload::worker::has_session_draft();
        let wait_for_initial_session_configured =
            !draft_session && Self::should_wait_for_initial_session(&session_selection);
        let should_prompt_for_paused_goal_after_startup_resume =
            Self::should_prompt_for_paused_goal_after_startup_resume(
                &session_selection,
                &initial_prompt,
                &initial_images,
            );
        let thread_and_widget_started_at = Instant::now();
        let pending_startup_thread_start = !draft_session
            && matches!(
                &session_selection,
                SessionSelection::StartFresh | SessionSelection::Exit
            );
        let mut start_in_agents_overview =
            matches!(&session_selection, SessionSelection::AgentsOverview);
        let mut read_only_thread = false;
        let mut provider_error = None;
        let mut history_notice = None;
        let (mut chat_widget, initial_started_thread) = match session_selection {
            SessionSelection::StartFresh
            | SessionSelection::Exit
            | SessionSelection::AgentsOverview => {
                if pending_startup_thread_start {
                    spawn_startup_thread_start(
                        &app_server,
                        local_settings.clone(),
                        config.clone(),
                        launch_choices.clone(),
                        app_event_tx.clone(),
                        app_server.explicit_model_provider(&config),
                    );
                }
                // Count a startup tooltip once the initial chat widget can render it.
                let startup_tooltip_override = if start_in_agents_overview {
                    None
                } else {
                    match startup_draft
                        .run_until(
                            tui,
                            prepare_startup_tooltip_override(
                                &mut local_settings,
                                &available_models,
                                is_first_run,
                            ),
                        )
                        .await
                    {
                        Ok(tooltip_override) => tooltip_override,
                        Err(err) => {
                            return shutdown_on_startup_error(app_server, err).await;
                        }
                    }
                };
                let init = crate::chatwidget::ChatWidgetInit {
                    local_settings: local_settings.clone(),
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    workspace_command_runner: Some(workspace_command_runner.clone()),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    account: account.clone(),
                    backend_authority: crate::app_server_session::BackendAuthority::Unreported,
                    model_catalog: model_catalog.clone(),
                    is_first_run,
                    model: model.clone(),
                    startup_tooltip_override,
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    terminal_title_invalid_items_warned: terminal_title_invalid_items_warned
                        .clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                let mut chat_widget = ChatWidget::new_with_app_event(init);
                if draft_session {
                    chat_widget.session_draft = Some(crate::chatwidget::SessionDraft::Editing);
                    chat_widget.set_reasoning_effort(config.model_reasoning_effort.clone());
                }
                chat_widget.set_queue_submissions_until_session_configured(
                    /*queue*/ pending_startup_thread_start,
                );
                (chat_widget, None)
            }
            SessionSelection::Resume(target_session) => {
                let rejoining = crate::idle_restart::rejoins(target_session.thread_id);
                if !rejoining
                    && app_server_target.thread_params_mode()
                        == crate::app_server_session::ThreadParamsMode::Remote
                    && config_persistence::has_explicit_resume_permission_override(
                        &config,
                        &harness_overrides,
                    )
                {
                    return shutdown_on_startup_error(
                        app_server,
                        color_eyre::eyre::eyre!(
                            "Permission overrides are not supported when resuming a remote task."
                        ),
                    )
                    .await;
                }
                if let Some(history_mode) = target_session.history_mode {
                    app_server.remember_thread_history_mode(target_session.thread_id, history_mode);
                }
                let model_settings = if rejoining {
                    crate::app_server_session::ResumeModelSettings::PreserveExistingThread
                } else {
                    config_persistence::resume_model_settings_for_overrides(
                        &config,
                        &harness_overrides,
                    )
                };
                let resumed = match startup_draft
                    .run_until(
                        tui,
                        app_server.resume_thread_with_permission_overrides(
                            &local_settings,
                            config.clone(),
                            target_session.thread_id,
                            model_settings,
                            crate::resume_permissions::ResumePermissions::from_overrides(
                                &config,
                                &harness_overrides,
                            ),
                        ),
                    )
                    .await
                {
                    Ok(Ok(resumed)) => Ok(resumed),
                    Ok(Err(err))
                        if crate::app_server_session::is_active_writer_error(&err)
                            || crate::app_server_session::model_provider_error(&err).is_some() =>
                    {
                        provider_error = crate::app_server_session::model_provider_error(&err);
                        read_only_thread = true;
                        match startup_draft
                            .run_until(
                                tui,
                                app_server.read_thread_for_viewing(
                                    &config,
                                    &local_settings,
                                    target_session.thread_id,
                                ),
                            )
                            .await
                        {
                            Ok(result) => result.map(|(thread, notice)| {
                                history_notice = notice;
                                thread
                            }),
                            Err(err) => {
                                return shutdown_on_startup_error(app_server, err).await;
                            }
                        }
                    }
                    Ok(resumed) => resumed,
                    Err(err) => {
                        return shutdown_on_startup_error(app_server, err).await;
                    }
                };
                let resumed = if read_only_thread {
                    match resumed {
                        Ok(resumed) => Some(resumed),
                        Err(err) => {
                            return shutdown_on_startup_error(app_server, err).await;
                        }
                    }
                } else {
                    let action = SessionStartAction::Resume(
                        model_settings,
                        crate::resume_permissions::ResumePermissions::from_overrides(
                            &config,
                            &harness_overrides,
                        ),
                    );
                    let resumed = complete_session_start(
                        &mut app_server,
                        SessionStartConfig {
                            config: &config,
                            local_settings: &local_settings,
                        },
                        &target_session,
                        action,
                        resumed,
                        async || {
                            startup_draft.flush_pending_events(tui).await?;
                            run_unarchive_prompt(tui, target_session.thread_id, action).await
                        },
                    )
                    .await;
                    match startup_try!(resumed) {
                        SessionStartOutcome::Started(started) => Some(*started),
                        SessionStartOutcome::CommandCenter => None,
                        SessionStartOutcome::Exit => {
                            return Ok(cancel_startup_sessions(app_server).await);
                        }
                    }
                };
                let acquired_model = resumed
                    .as_ref()
                    .map(|started| started.session.model.clone())
                    .filter(|model| !model.is_empty());
                config.model.clone_from(&acquired_model);
                let result = handle_model_migration_prompt_if_needed(
                    tui,
                    &mut config,
                    &local_settings,
                    &mut launch_choices,
                    acquired_model.as_deref(),
                    &app_event_tx,
                    &available_models,
                )
                .await;
                if let Some(exit_info) = startup_try!(result) {
                    shutdown_startup_sessions([app_server]).await;
                    return Ok(exit_info);
                }
                let init = crate::chatwidget::ChatWidgetInit {
                    local_settings: local_settings.clone(),
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    workspace_command_runner: Some(workspace_command_runner.clone()),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    )
                    .filter(|_| resumed.is_some()),
                    enhanced_keys_supported,
                    account: account.clone(),
                    backend_authority: crate::app_server_session::BackendAuthority::Unreported,
                    model_catalog: model_catalog.clone(),
                    is_first_run,
                    model: config.model.clone(),
                    startup_tooltip_override: None,
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    terminal_title_invalid_items_warned: terminal_title_invalid_items_warned
                        .clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                (ChatWidget::new_with_app_event(init), resumed)
            }
            SessionSelection::Fork(target_session) => {
                let explicit_permission_override =
                    config_persistence::has_explicit_resume_permission_override(
                        &config,
                        &harness_overrides,
                    );
                if explicit_permission_override
                    && app_server_target.thread_params_mode()
                        == crate::app_server_session::ThreadParamsMode::Remote
                {
                    return shutdown_on_startup_error(
                        app_server,
                        color_eyre::eyre::eyre!(
                            "Permission overrides are not supported when forking a remote task."
                        ),
                    )
                    .await;
                }
                let permission_mode = if explicit_permission_override {
                    crate::app_server_session::ForkPermissionMode::OverrideFromCurrentConfig
                } else {
                    crate::app_server_session::ForkPermissionMode::InheritSaved
                };
                session_telemetry.counter(
                    "codex.thread.fork",
                    /*inc*/ 1,
                    &[("source", "cli_subcommand")],
                );
                let forked = match startup_draft
                    .run_until(
                        tui,
                        app_server.fork_thread_with_permission_mode(
                            &local_settings,
                            config.clone(),
                            target_session.thread_id,
                            permission_mode,
                        ),
                    )
                    .await
                {
                    Ok(forked) => forked,
                    Err(err) => {
                        return shutdown_on_startup_error(app_server, err).await;
                    }
                };
                let action = SessionStartAction::Fork(permission_mode);
                let forked = complete_session_start(
                    &mut app_server,
                    SessionStartConfig {
                        config: &config,
                        local_settings: &local_settings,
                    },
                    &target_session,
                    action,
                    forked,
                    async || {
                        startup_draft.flush_pending_events(tui).await?;
                        run_unarchive_prompt(tui, target_session.thread_id, action).await
                    },
                )
                .await;
                let forked = match startup_try!(forked) {
                    SessionStartOutcome::Started(started) => Some(*started),
                    SessionStartOutcome::CommandCenter => None,
                    SessionStartOutcome::Exit => {
                        return Ok(cancel_startup_sessions(app_server).await);
                    }
                };
                let acquired_model = forked
                    .as_ref()
                    .map(|started| started.session.model.clone())
                    .filter(|model| !model.is_empty());
                config.model.clone_from(&acquired_model);
                let result = handle_model_migration_prompt_if_needed(
                    tui,
                    &mut config,
                    &local_settings,
                    &mut launch_choices,
                    acquired_model.as_deref(),
                    &app_event_tx,
                    &available_models,
                )
                .await;
                if let Some(exit_info) = startup_try!(result) {
                    shutdown_startup_sessions([app_server]).await;
                    return Ok(exit_info);
                }
                if let Some(forked) = &forked
                    && config.model_reasoning_effort.is_none()
                {
                    config.model_reasoning_effort = forked.session.reasoning_effort.clone();
                }
                let init = crate::chatwidget::ChatWidgetInit {
                    local_settings: local_settings.clone(),
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    workspace_command_runner: Some(workspace_command_runner.clone()),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    )
                    .filter(|_| forked.is_some()),
                    enhanced_keys_supported,
                    account: account.clone(),
                    backend_authority: crate::app_server_session::BackendAuthority::Unreported,
                    model_catalog: model_catalog.clone(),
                    is_first_run,
                    model: config.model.clone(),
                    startup_tooltip_override: None,
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    terminal_title_invalid_items_warned: terminal_title_invalid_items_warned
                        .clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                (ChatWidget::new_with_app_event(init), forked)
            }
        };
        let startup_session_cancelled = initial_started_thread.is_none()
            && !draft_session
            && !pending_startup_thread_start
            && !start_in_agents_overview;
        if startup_session_cancelled {
            start_in_agents_overview = true;
        }
        chat_widget.note_rendered_width(tui.terminal.last_known_screen_size.width);
        if pending_startup_thread_start && !start_in_agents_overview {
            chat_widget
                .empty_state_animation
                .borrow_mut()
                .continue_from(&mut startup_draft.blossom.borrow_mut());
        }
        chat_widget.remote_connection = remote_connection;
        chat_widget.snapshot_local_images = app_server_target.uses_remote_workspace();
        let thread_and_widget_ms = thread_and_widget_started_at.elapsed().as_millis();
        chat_widget.windows_sandbox_local_server = !app_server_target.uses_remote_workspace()
            && app_server.app_server_platform_os() == Some("windows");
        chat_widget.windows_sandbox_host = if app_server_target.uses_remote_workspace() {
            WindowsSandboxHost::Remote
        } else {
            initial_started_thread
                .as_ref()
                .map_or(WindowsSandboxHost::Unknown, |started| {
                    started.session.windows_sandbox_host
                })
        };
        // This launch warning belongs to the TUI, independent of picker/trust client replacement.
        if let Some(warning) = daemon_startup_warning {
            app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::StartupWarningsCell::new(vec![warning]),
            )));
        }
        // Automatic NFS detection is Linux-only; manual migration supports Unix.
        #[cfg(target_os = "linux")]
        if !app_server_target.uses_remote_workspace() {
            tokio::spawn(startup_warnings::warn_about_nfs_migration(
                config.sqlite_config().clone(),
                app_event_tx.clone(),
            ));
        }
        let file_search = FileSearchManager::new(config.cwd.to_path_buf(), app_event_tx.clone());
        let runtime_keymap = RuntimeKeymap::for_session(
            &local_settings.tui.keymap,
            local_settings.workspace_enabled(&config),
        )
        .map_err(|err| {
            color_eyre::eyre::eyre!(
                "Invalid `tui.keymap` configuration: {err}\n\
Fix the config and retry.\n\
See the Codex keymap documentation for supported actions and examples."
            )
        });
        let runtime_keymap = startup_try!(runtime_keymap);
        #[cfg(not(debug_assertions))]
        let upgrade_version = crate::updates::get_upgrade_version(&config);

        let agents_overview =
            agents_overview::AgentsOverviewState::new(local_settings.tui.agents_overview_grouping);
        let mut app = Self {
            reload_requested: false,
            farm: Default::default(),
            theme: Default::default(),
            jira: None,
            provider_writes,
            feature_write_lock: Arc::default(),
            config_notification_test_generation: 0,
            config_notification_test_pending: None,
            model_catalog,
            session_telemetry: session_telemetry.clone(),
            terminal_size_tags: tui.terminal_size_tags.clone(),
            app_event_tx,
            chat_widget,
            workspace_command_runner: Some(workspace_command_runner),
            config,
            local_settings,
            launch_cwd,
            local_shell,
            runtime_working_directory_override: None,
            cli_kv_overrides,
            harness_overrides,
            loader_overrides,
            cloud_config_bundle,
            runtime_approval_policy_override: None,
            runtime_approvals_reviewer_override: None,
            runtime_permission_profile_override: None,
            pending_server_profiles: HashMap::new(),
            file_search,
            enhanced_keys_supported,
            keymap: runtime_keymap,
            key_chord_matcher: KeyChordMatcher::default(),
            routed_key_activation: None,
            transcript_cells: Vec::new(),
            code_mode_transcript: Default::default(),
            live_work: Default::default(),
            native_history: Default::default(),
            turn_tips: Default::default(),
            transcript_view: Default::default(),
            conversation_frame: Default::default(),
            last_rendered_history_tail: None,
            last_thread_usage_status_cell: None,
            pending_thread_usage_history_refresh: false,
            overlay: None,
            retained_analytics: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            transcript_reflow: TranscriptReflowState::default(),
            initial_history_replay_buffer: None,
            pending_thread_switch_resets: 0,
            scrollback_has_older_history: false,
            commit_animation: None,
            status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: terminal_title_invalid_items_warned.clone(),
            skill_load_warnings: SkillLoadWarningState::default(),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            environment_manager,
            app_server_target,
            pending_mouse_paste: None,
            mouse_paste_environment: super::mouse_paste::PasteEnvironment::detect(),
            reconnect: ReconnectState {
                seen_version_notice: initial_server_version_notice
                    .as_ref()
                    .map(|(_, key)| key.clone()),
                ..Default::default()
            },
            pending_shutdown_exit_thread_id: None,
            windows_sandbox: WindowsSandboxState {
                prompt_after_trust: should_prompt_windows_sandbox_nux_at_startup
                    || startup_defaults.prompt_windows_sandbox,
                ..Default::default()
            },
            thread_event_channels: HashMap::new(),
            thread_delivery: super::thread_delivery::ThreadDelivery::new(),
            pending_realtime_speech_replay: HashMap::new(),
            pending_realtime_transcript_replay: HashMap::new(),
            realtime_replay_order: VecDeque::new(),
            background_voice: None,
            background_voice_error: None,
            temporary_structured_requests: HashMap::new(),
            pending_thread_titles: HashMap::new(),
            model_selection_generation: Default::default(),
            thread_event_listener_tasks: HashMap::new(),
            agent_navigation: AgentNavigationState::default(),
            agents_overview,
            side_threads: HashMap::new(),
            active_export: None,
            pending_revert: None,
            rewind: None,
            saved_openings: Default::default(),
            sidebars: HashMap::new(),
            side_companions: HashMap::new(),
            zoomed_thread: None,
            exact_sidebar_width: None,
            divider_drag: None,
            pane_resize: None,
            passive_link_drag: None,
            abandoned_side_threads: HashSet::new(),
            active_thread_id: None,
            active_thread_rx: None,
            primary_thread_id: None,
            last_subagent_backfill_attempt: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
            pending_app_server_requests: PendingAppServerRequests::default(),
            pending_startup_thread_start,
            pending_server_version_notice: if pending_startup_thread_start {
                initial_server_version_notice
                    .as_ref()
                    .map(|(notice, _)| notice.clone())
            } else {
                None
            },
            pending_open_resume_picker: false,
            pending_working_directory_change: None,
            startup_protected_input_boundary: true,
            startup_pending_protected_request: false,
            rate_limit_hard_stop_generation: 0,
            account_ui: Default::default(),
            rate_limit_refresh_state: Default::default(),
            queued_messages: Default::default(),
            server_operations: Default::default(),
            pending_mcp_login_start: None,
            active_mcp_login_ids: HashMap::new(),
            pending_plugin_enabled_writes: HashMap::new(),
            pending_hook_enabled_writes: HashMap::new(),
            recap: recap::RecapState::default(),
            #[cfg(test)]
            _test_codex_home: None,
        };
        if tui.is_owned_screen()
            && app.mouse_paste_environment.wsl
            && app.mouse_paste_environment.allows(
                app.local_settings.tui.right_click_paste,
                app.local_settings.transcript_mode,
            )
        {
            // Prewarm WSL interop off the UI thread, without accessing clipboard contents.
            let _ = tui.clipboard.warm_text_reader(tui.frame_requester());
        }
        app.remember_launch_permissions();
        if !tui.is_terminal_focused() {
            app.recap.note_focus_lost(Instant::now());
        }
        let _ =
            app.initialize_server_version_notice(CODEX_CLI_VERSION, app_server.server_version());
        if initial_server_version_notice.is_none() {
            app.update_server_version_overview_notice(
                CODEX_CLI_VERSION,
                /*server_version*/ None,
            );
        }
        if start_in_agents_overview {
            app.open_agents_overview(&app_server);
        } else {
            app.refresh_agents_overview_threads(&app_server);
        }
        if let Some(entry) = startup_hooks_browser {
            app.chat_widget.open_hooks_browser(entry);
        }
        app.update_visible_history_rows(tui.terminal.last_known_screen_size);
        let initial_session_started_at = Instant::now();
        #[cfg(target_os = "linux")]
        if crate::reload::worker::has_unattached_input() {
            app.restore_reload_input(/*thread*/ None).await?;
        }
        if let Some(started) = initial_started_thread {
            let thread_id = started.session.thread_id;
            app.chat_widget
                .set_task_mentions_enabled(started.task_tools_available);
            if started.can_accept_text_input {
                app.agent_navigation.text_input_threads.insert(thread_id);
            }
            if started.blocks_direct_input {
                app.mark_primary_thread_parent_owned(thread_id);
            }
            if let Some(error) = provider_error.clone() {
                app.chat_widget
                    .show_provider_error(error, /*rejected_message_id*/ None);
            } else if read_only_thread {
                app.chat_widget.show_external_writer_thread();
            }
            match startup_draft
                .run_until(
                    tui,
                    app.enqueue_primary_thread_session(started.session, started.turns),
                )
                .await
            {
                Ok(result) => startup_try!(result),
                Err(err) => {
                    return shutdown_on_startup_error(app_server, err).await;
                }
            }
            if let Some(notice) = started.history_notice {
                app.chat_widget.add_info_message(notice, /*hint*/ None);
            }
            if let Some(error) = provider_error {
                app.chat_widget.show_provider_error(error, None);
            } else if read_only_thread {
                app.ensure_thread_channel(thread_id).mark_external_writer();
                app.chat_widget.show_external_writer_thread();
            }
            if let Some(notice) = history_notice {
                app.chat_widget
                    .add_info_message(notice.to_string(), /*hint*/ None);
            }
            if !read_only_thread
                && should_prompt_for_paused_goal_after_startup_resume
                && let Err(err) = startup_draft
                    .run_until(
                        tui,
                        app.maybe_prompt_resume_paused_goal_after_resume(
                            &mut app_server,
                            thread_id,
                        ),
                    )
                    .await
            {
                return shutdown_on_startup_error(app_server, err).await;
            }
        }
        if !start_in_agents_overview
            && !pending_startup_thread_start
            && let Some((notice, _)) = &initial_server_version_notice
        {
            app.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::new_server_version_warning(notice.clone()),
            )));
        }
        let initial_session_ms = initial_session_started_at.elapsed().as_millis();

        if let Err(err) = startup_draft.flush_pending_events(tui).await {
            return shutdown_on_startup_error(app_server, err).await;
        }
        if Self::should_handle_active_thread_events(
            wait_for_initial_session_configured,
            app.active_thread_rx.is_some(),
        ) && let Err(err) = app.drain_active_thread_events(tui).await
        {
            return shutdown_on_startup_error(app_server, err).await;
        }
        if app_event_rx.is_empty()
            && !app.has_queued_startup_protected_request()
            && !app.startup_pending_protected_request
            && !app.chat_widget.has_active_view()
            && !app.chat_widget.has_pending_protected_request()
            && let Err(err) = startup_draft.flush_pending_paste_newline(tui).await
        {
            return shutdown_on_startup_error(app_server, err).await;
        }
        // Keep cancelled resume/fork text editable, but never carry confirmation to another session.
        let mut pending_startup_submission =
            startup_draft.take_submission_intent() && !startup_session_cancelled;
        let mut pending_startup_draft = Some(startup_draft.into_draft());
        if app_event_rx.is_empty() && !app.has_queued_startup_protected_request() {
            app.chat_widget.restore_startup_input_when_ready(
                &mut pending_startup_draft,
                &mut pending_startup_submission,
            );
        }

        #[cfg(windows)]
        let mut terminal_color_probe_pending = true;
        #[cfg(windows)]
        if app.ready_for_terminal_color_probe(!app_event_rx.is_empty()) {
            tui.probe_default_colors_after_protected_startup();
            terminal_color_probe_pending = false;
        }

        let event_stream_started_at = Instant::now();
        tui.schedule_screen_size_recheck(Duration::ZERO);
        if let Err(err) = app.render_startup_frame(tui, &app_event_rx) {
            return shutdown_on_startup_error(app_server, err).await;
        }
        let tui_events = tui.event_stream();
        let mut output_completions = crate::custom_terminal::output::completion_events();
        app.complete_terminal_output();
        tokio::pin!(tui_events);
        #[cfg(target_os = "linux")]
        let mut desktop_attention = tui.desktop_attention_receiver();
        tracing::info!(
            duration_ms = %(startup_elapsed_before_app + startup_started_at.elapsed()).as_millis(),
            bootstrap_ms = %bootstrap_ms,
            thread_and_widget_ms = %thread_and_widget_ms,
            initial_session_ms = %initial_session_ms,
            event_stream_ms = %event_stream_started_at.elapsed().as_millis(),
            "tui startup initial frame scheduled"
        );
        if matches!(
            account,
            crate::app_server_session::BootstrapAccount::AwaitingResume
        ) && let Some(thread_id) = app.current_displayed_thread_id()
        {
            app.app_event_tx
                .send(AppEvent::RefreshConversationAccount { thread_id });
        }
        app.refresh_skills(&app_server);
        // Kick off a non-blocking rate-limit prefetch so the first `/status`
        // already has data and available reset credits can be surfaced, without
        // delaying the initial frame render.
        if app.chat_widget.has_codex_backend_auth()
            && matches!(&account, crate::app_server_session::BootstrapAccount::Loaded(account)
                if account.requires_openai_auth && account.has_chatgpt_account())
        {
            crate::security_setup::prefetch(
                &app.config,
                &app_server,
                app.app_event_tx.clone(),
                app.chat_widget.security_setup_request_id,
            );
            let reset_hint_request_id = app.chat_widget.start_rate_limit_reset_startup_check();
            app.refresh_rate_limits(
                &app_server,
                RateLimitRefreshOrigin::StartupPrefetch {
                    reset_hint_request_id,
                },
            );
        }

        let mut schedule = ui_schedule::UiSchedule::new();
        app.transcript_view
            .enable_background_preparation(tui.frame_requester());
        #[cfg(target_os = "linux")]
        let ui_host_ready = !crate::reload::worker::is_hosted();
        let mut idle_restart = crate::idle_restart::IdleRestart::new();
        let waiting_for_initial_session_configured = wait_for_initial_session_configured;

        #[cfg(not(debug_assertions))]
        let pre_loop_exit_reason = if let Some(latest_version) = upgrade_version {
            let control = Box::pin(app.handle_event(
                tui,
                &mut app_server,
                AppEvent::InsertHistoryCell(Box::new(UpdateAvailableHistoryCell::new(
                    latest_version,
                    crate::update_action::get_update_action(),
                ))),
            ))
            .await;
            match startup_try!(control) {
                AppRunControl::Continue => None,
                AppRunControl::Exit(exit_reason) => Some(exit_reason),
            }
        } else {
            None
        };
        #[cfg(debug_assertions)]
        let pre_loop_exit_reason: Option<ExitReason> = None;

        let mut backends = super::backend_sessions::BackendSession::new(app_server);
        drop(startup_stage);
        let exit_reason_result = if let Some(exit_reason) = pre_loop_exit_reason {
            Ok(exit_reason)
        } else {
            ui_schedule::drive(
                &mut app,
                tui,
                &mut backends,
                launch_choices.clone(),
                &mut app_event_rx,
                tui_events.as_mut().get_mut(),
                &mut pending_startup_draft,
                &mut pending_startup_submission,
                &mut schedule,
                &mut output_completions,
                #[cfg(target_os = "linux")]
                &mut desktop_attention,
                #[cfg(target_os = "linux")]
                ui_host_ready,
                &mut idle_restart,
                waiting_for_initial_session_configured,
                #[cfg(windows)]
                terminal_color_probe_pending,
            )
            .await?
        };
        let exit_reason_result = match app.shutdown_local_shell(&mut app_event_rx).await {
            Some(recovery) => Err(match exit_reason_result {
                Ok(_) => color_eyre::eyre::eyre!(recovery),
                Err(error) => error.wrap_err(recovery),
            }),
            None => exit_reason_result,
        };
        // Let the pending terminal poll unwind while the app server shuts down.
        tui.pause_events();
        app.chat_widget.mention_usage_mut().flush().await;
        if let Err(err) = backends.shutdown().await {
            tracing::warn!(error = %err, "failed to shut down embedded app server");
        }
        let clear_result = tui.terminal.clear();
        // Keep the alternate screen active until the outer guard restores both keyboard stacks.
        let exit_reason = match exit_reason_result {
            Ok(exit_reason) => {
                clear_result?;
                exit_reason
            }
            Err(err) => {
                if let Err(clear_err) = clear_result {
                    tracing::warn!(error = %clear_err, "failed to clear terminal UI");
                }
                return Err(err);
            }
        };
        Ok(app.exit_info(exit_reason))
    }
}
