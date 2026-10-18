//! Configuration request tails, background refreshes, and accepted-state projection.

use super::*;
use crate::app_server_session::typed_requests::SessionRpc;

pub(super) async fn build_config_on_runtime_worker_continuation(
    builder: ConfigBuilder,
    application_network_policy: codex_http_client::NetworkPolicy,
    error_context: String,
) -> Result<Config> {
    // Tokio stores the task output inline even when it boxes the future. Keep the large
    // Config off the caller's stack while Tokio allocates the task during session switches.
    match tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        builder.build().await.map(|mut config| {
            config.application_network_policy = application_network_policy;
            Box::new(config)
        })
    }))
    .await
    {
        Ok(build_result) => build_result.map(|config| *config).wrap_err(error_context),
        Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
        Err(err) => Err(err).wrap_err_with(|| format!("{error_context} task failed")),
    }
}

impl App {
    pub(in crate::app) fn config_builder_for_cwd(&self, cwd: PathBuf) -> ConfigBuilder {
        let mut overrides = self.harness_overrides.clone();
        overrides.cwd = Some(cwd);
        ConfigBuilder::default()
            .codex_home(self.config.codex_home.to_path_buf())
            .cli_overrides(self.cli_kv_overrides.clone())
            .harness_overrides(overrides)
            .loader_overrides(self.loader_overrides.clone())
            .cloud_config_bundle(self.cloud_config_bundle.clone())
    }

    pub(super) async fn rebuild_config_for_cwd_continuation(&self, cwd: PathBuf) -> Result<Config> {
        let builder = self.config_builder_for_cwd(cwd.clone());
        build_config_on_runtime_worker(
            builder,
            self.config.application_network_policy.clone(),
            format!("Failed to rebuild config for cwd {}", cwd.display()),
        )
        .await
    }

    pub(super) async fn select_permission_profile_continuation(
        &mut self,
        app_server: &mut AppServerSession,
        mut selection: PermissionProfileSelection,
        thread_id: ThreadId,
        params: ThreadSettingsUpdateParams,
        fallback_policy: AskForApproval,
        fallback_reviewer: ApprovalsReviewer,
    ) {
        self.request_server(
            app_server,
            turn_submissions::ui_requests::UiRequestScope::Thread(thread_id),
            turn_submissions::ui_requests::UiRequestKind::Mutation,
            "Selecting permissions",
            move |handle| async move {
                handle
                    .thread_settings_update(
                        codex_app_server_protocol::RequestId::String(
                            Uuid::new_v4().to_string(),
                        ),
                        params,
                    )
                    .await
            },
            move |app, server, result| {
                let displayed = app.chat_widget.thread_id() == Some(thread_id);
                match result {
                    Ok(true) => {
                        // Omission preserves the preceding accepted value, including an
                        // earlier request whose notification has not reached the UI yet.
                        let previous = app
                            .agents_overview
                            .requested_permission_profiles
                            .get(&thread_id);
                        let (policy, reviewer) = if displayed {
                            let config = app.chat_widget.config_ref();
                            (
                                config.permissions.approval_policy.value().into(),
                                config.approvals_reviewer,
                            )
                        } else {
                            (fallback_policy, fallback_reviewer)
                        };
                        selection.approval_policy = selection
                            .approval_policy
                            .or_else(|| previous.and_then(|profile| profile.approval_policy))
                            .or(Some(policy));
                        selection.approvals_reviewer = selection
                            .approvals_reviewer
                            .or_else(|| previous.and_then(|profile| profile.approvals_reviewer))
                            .or(Some(reviewer));
                        app.agents_overview
                            .selected_permission_profiles
                            .insert(thread_id, selection.profile_id.clone());
                        app.agents_overview
                            .requested_permission_profiles
                            .insert(thread_id, selection.clone());
                        if displayed {
                            app.chat_widget.add_info_message(
                                format!(
                                    "Permission selection requested: {}",
                                    selection.display_label
                                ),
                                None,
                            );
                            app.chat_widget.submit_initial_user_message_if_pending();
                        }
                    }
                    Ok(false) => {
                        server.note_operation_settings_unsupported();
                        if displayed {
                            app.chat_widget
                                .retain_input_after_failed_permission_selection();
                            app.chat_widget.add_error_message(
                                "Permission selection requires a newer app server.".into(),
                            );
                        }
                    }
                    Err(error) => {
                        if displayed {
                            app.chat_widget
                                .retain_input_after_failed_permission_selection();
                            app.chat_widget.add_error_message(format!(
                                "Failed to select permissions: {error}"
                            ));
                        }
                    }
                }
                Ok(())
            },
        );
    }

    pub(in crate::app) fn install_refreshed_config(&mut self, mut config: Config) -> Result<()> {
        self.apply_runtime_policy_overrides(&mut config, RuntimePolicyOverrideScope::All)?;
        self.local_settings = self.local_settings.reloaded(&config);
        self.refresh_server_version_overview_notice(CODEX_CLI_VERSION);
        self.chat_widget
            .local_settings
            .tui
            .terminal_resize_reflow_max_rows =
            self.local_settings.tui.terminal_resize_reflow_max_rows;
        self.config = config;
        self.chat_widget.sync_plugin_mentions_config(&self.config);
        Ok(())
    }

    pub(in crate::app) fn queue_config_refresh<A>(&mut self, action: &'static str, apply: A)
    where
        A: FnOnce(&mut App, &mut AppServerSession, Result<()>) -> Result<()> + Send + 'static,
    {
        let cwd = self.chat_widget.config_ref().cwd.to_path_buf();
        let origin = self.current_displayed_thread_id();
        let baseline = self.config.clone();
        let builder = self.config_builder_for_cwd(cwd.clone());
        let network_policy = self.config.application_network_policy.clone();
        self.request_local(
            "Refresh local configuration",
            async move {
                build_config_on_runtime_worker(
                    builder,
                    network_policy,
                    format!("Failed to rebuild config for cwd {}", cwd.display()),
                )
                .await
            },
            move |app, server, result| {
                if app.current_displayed_thread_id() != origin {
                    return Ok(());
                }
                let result = if app.config == baseline {
                    result.and_then(|config| app.install_refreshed_config(config))
                } else {
                    // A newer local selection wins over an earlier disk snapshot.
                    result.map(|_| ())
                };
                if let Err(error) = &result {
                    tracing::warn!(%error, action, "failed to refresh local configuration");
                }
                apply(app, server, result)
            },
        );
    }

    pub(super) fn read_effective_config_after_overridden_write_continuation<A>(
        &mut self,
        server: &AppServerSession,
        setting: &'static str,
        cwd: String,
        apply: A,
    ) where
        A: FnOnce(&mut App, &mut AppServerSession, ConfigReadResponse) -> Result<()>
            + Send
            + 'static,
    {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        self.request_server(
                server,
                UiRequestScope::Connection,
                UiRequestKind::ReadOnly,
                "Read effective configuration",
                move |handle| crate::config_update::read_effective_config_typed(handle, cwd),
                move |app, server, result| match result {
                    Ok(response) => apply(app, server, response),
                    Err(error) => {
                        app.chat_widget.add_error_message(format!(
                            "{setting} were saved, but Codex could not refresh the effective config: {error}"
                        ));
                        Ok(())
                    }
                },
            );
    }

    pub(super) async fn update_feature_flags_continuation(
        &mut self,
        app_server: &mut AppServerSession,
        next_config: Config,
        approval_policy_override: Option<AskForApproval>,
        approvals_reviewer_override: Option<ApprovalsReviewer>,
        permission_profile_override: Option<PermissionProfile>,
        active_permission_profile_override: Option<ActivePermissionProfile>,
        feature_updates_to_apply: Vec<(Feature, bool)>,
        permissions_history_label: Option<&'static str>,
        config_edits: Vec<codex_app_server_protocol::ConfigEdit>,
    ) {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        let origin = self.current_displayed_thread_id();
        let cwd = self.chat_widget.config_ref().cwd.display().to_string();
        self.request_server(
                app_server,
                UiRequestScope::Connection,
                UiRequestKind::Mutation,
                "Save experimental features",
                move |handle| crate::config_update::write_config_batch_typed(handle, config_edits),
                move |app, server, result| {
                    let write_response = match result {
                        Ok(response) => response,
                        Err(error) => {
                            app.chat_widget.add_error_message(format!("Failed to update experimental features: {error}"));
                            return Ok(());
                        }
                    };
                    if write_response.status == WriteStatus::OkOverridden {
                        let message = overridden_write_message(&write_response);
                        app.chat_widget.add_error_message(format!("Experimental feature changes were saved but not applied: {message}"));
                        app.read_effective_config_after_overridden_write(server, "Experimental feature changes", cwd,
                            move |app, server, effective_config| {
                                if app.current_displayed_thread_id() != origin {
                                    app.queue_background_auto_review_settings(server, origin, &effective_config, &feature_updates_to_apply);
                                    return Ok(());
                                }
                                app.sync_feature_state_from_effective_config(&effective_config, &feature_updates_to_apply);
                                app.sync_auto_review_runtime_state_from_effective_config(server, &effective_config, &feature_updates_to_apply);
                                Ok(())
                            });
                        return Ok(());
                    }
                    if app.current_displayed_thread_id() != origin {
                        if let Some(thread_id) = origin
                            && (approval_policy_override.is_some() || approvals_reviewer_override.is_some() || permission_profile_override.is_some()) {
                            let params = ThreadSettingsUpdateParams {
                                thread_id: thread_id.to_string(),
                                approval_policy: approval_policy_override,
                                approvals_reviewer: approvals_reviewer_override.map(codex_app_server_protocol::ApprovalsReviewer::from),
                                permissions: active_permission_profile_override.as_ref().map(|profile| profile.id.clone()),
                                ..Default::default()
                            };
                            app.enqueue_server_operation(server, thread_id, turn_submissions::ServerOperation::Mutation(turn_submissions::ThreadMutation::Settings(Box::new(params))));
                        }
                        app.chat_widget.add_info_message("Experimental features saved. Runtime changes still target the original conversation.".into(), None);
                        return Ok(());
                    }
                    let memory_tool_was_enabled = app.config.features.enabled(Feature::MemoryTool);
                    for (feature, enabled) in &feature_updates_to_apply {
                        if let Err(error) = app.config.features.set_enabled(*feature, *enabled) {
                            app.chat_widget.add_error_message(format!("Failed to apply feature state: {error}"));
                            return Ok(());
                        }
                    }
                    if approval_policy_override.is_some() || permission_profile_override.is_some() {
                        app.config.permissions = next_config.permissions;
                    }
                    if approvals_reviewer_override.is_some() {
                        app.config.approvals_reviewer = next_config.approvals_reviewer;
                    }
                    let show_memory_enable_notice =
                        feature_updates_to_apply.iter().any(|(feature, enabled)| {
                            *feature == Feature::MemoryTool && *enabled && !memory_tool_was_enabled
                        },
                    );
                    for (feature, effective_enabled) in feature_updates_to_apply {
                        app.chat_widget
                            .set_feature_enabled(feature, effective_enabled);
                    }
                    if show_memory_enable_notice {
                        app.chat_widget.add_memories_enable_notice();
                    }
                    if approvals_reviewer_override.is_some() {
                        app.runtime_approvals_reviewer_override = approvals_reviewer_override;
                        app.set_approvals_reviewer_in_app_and_widget(app.config.approvals_reviewer);
                    }
                    if approval_policy_override.is_some() {
                        app.chat_widget.set_approval_policy(AskForApproval::from(
                            app.config.permissions.approval_policy.value(),
                        ));
                    }
                    let permission_profile_override_value = permission_profile_override
                        .is_some()
                        .then(|| app.config.permissions.permission_profile().clone());
                    if let Some(permission_profile) = permission_profile_override_value.as_ref()
                        && let Err(err) = app
                            .chat_widget
                            .set_permission_profile_from_session_snapshot(
                                PermissionProfileSnapshot::from_session_snapshot(
                                    permission_profile.clone(),
                                    active_permission_profile_override.clone(),
                                ),
                            )
                    {
                        tracing::error!(
                            error = %err,
                            "failed to set auto-review permission profile on chat config"
                        );
                        app.chat_widget
                            .add_error_message(format!("Failed to enable Approve for me: {err}"));
                    }
                    if permission_profile_override.is_some() {
                        app.runtime_approvals_reviewer_override = Some(app.config.approvals_reviewer);
                        app.runtime_permission_profile_override =
                            Some(RuntimePermissionProfileOverride::from_config(&app.config));
                    }

                    if approval_policy_override.is_some()
                        || approvals_reviewer_override.is_some()
                        || permission_profile_override.is_some()
                    {

                        // This uses `OverrideTurnContext` intentionally: toggling the
                        // experiment should update the active thread's effective approval
                        // settings immediately, just like a `/permissions` selection. Without
                        // this runtime patch, the config edit would only affect future
                        // sessions or turns recreated from disk.
                        let op = AppCommand::override_turn_context(
                            /*cwd*/ None,
                            approval_policy_override,
                            approvals_reviewer_override,
                            permission_profile_override,
                            active_permission_profile_override,
                            /*model*/ None,
                            /*effort*/ None,
                            /*summary*/ None,
                            /*service_tier*/ None,
                            /*collaboration_mode*/ None,
                        );
                        app.submit_permission_runtime_update(server, op);
                    }

                    if let Some(label) = permissions_history_label {
                        app.chat_widget.add_info_message(
                            format!("Permissions updated to {label}"),
                            /*hint*/ None,
                        );
                    }
                    Ok(())
                });
    }

    pub(super) fn queue_memory_settings(
        &mut self,
        server: &AppServerSession,
        use_memories: bool,
        generate_memories: bool,
    ) {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        let previous_generate = self.config.memories.generate_memories;
        let origin = self.current_displayed_thread_id();
        let cwd = self.chat_widget.config_ref().cwd.display().to_string();
        let edits =
            crate::config_update::build_memory_settings_edits(use_memories, generate_memories);
        self.request_server(
                server,
                UiRequestScope::Connection,
                UiRequestKind::Mutation,
                "Save memory settings", move |handle| crate::config_update::write_config_batch_typed(handle, edits),
                move |app, server, result| {
                    let response = match result {
                        Ok(response) => response,
                        Err(error) => {
                            app.app_event_tx.send(AppEvent::FollowTranscript);
                            app.chat_widget.add_error_message(format!("Failed to save memory settings: {error}"));
                            return Ok(());
                        }
                    };
                    if response.status == WriteStatus::OkOverridden {
                        app.app_event_tx.send(AppEvent::FollowTranscript);
                        app.chat_widget.add_error_message(format!("Memory setting changes were saved but not applied: {}", overridden_write_message(&response)));
                        app.read_effective_config_after_overridden_write(server, "Memory setting changes", cwd,
                            move |app, server, effective_config| {
                                let Some(memories) = memories_from_effective_config(&effective_config) else {
                                    app.chat_widget.add_error_message("Memory settings were saved, but their effective values could not be confirmed.".into());
                                    return Ok(());
                                };
                                let effective_generate = memories.generate_memories.unwrap_or(previous_generate);
                                if app.current_displayed_thread_id() == origin {
                                    app.sync_memory_state_from_effective_config(&effective_config);
                                }
                                app.queue_thread_memory_mode(server, origin, previous_generate, effective_generate);
                                Ok(())
                            });
                    } else {
                        if app.current_displayed_thread_id() == origin {
                            app.config.memories.use_memories = use_memories;
                            app.config.memories.generate_memories = generate_memories;
                            app.chat_widget.set_memory_settings(use_memories, generate_memories);
                        }
                        app.queue_thread_memory_mode(server, origin, previous_generate, generate_memories);
                    }
                    Ok(())
                },
            );
    }

    pub(super) fn queue_thread_memory_mode(
        &mut self,
        server: &AppServerSession,
        origin: Option<ThreadId>,
        previous_generate: bool,
        generate_memories: bool,
    ) {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        if generate_memories == previous_generate {
            return;
        }
        let Some(thread_id) = origin else {
            return;
        };
        let mode = if generate_memories {
            ThreadMemoryMode::Enabled
        } else {
            ThreadMemoryMode::Disabled
        };
        self.request_server(
            server,
            UiRequestScope::Thread(thread_id),
            UiRequestKind::Mutation,
            "Update thread memory mode",
            move |handle| async move {
                handle
                    .thread_memory_mode_set(
                        codex_app_server_protocol::RequestId::String(format!(
                            "tui-memory-mode-{}",
                            Uuid::new_v4()
                        )),
                        codex_app_server_protocol::ThreadMemoryModeSetParams {
                            thread_id: thread_id.to_string(),
                            mode,
                        },
                    )
                    .await
            },
            move |app, _, result| {
                if let Err(error) = result {
                    app.app_event_tx.send(AppEvent::FollowTranscript);
                    app.chat_widget.add_error_message(format!(
                        "Saved memory settings, but failed to update conversation {thread_id}: {error}"
                    ));
                }
                Ok(())
            },
        );
    }

    pub(super) async fn reset_memories_with_app_server_continuation(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        use super::super::turn_submissions::ui_requests::UiRequestKind;
        use super::super::turn_submissions::ui_requests::UiRequestScope;
        self.app_event_tx.send(AppEvent::FollowTranscript);
        self.request_server(
            app_server,
            UiRequestScope::Connection,
            UiRequestKind::Mutation,
            "Reset memories",
            |handle| async move {
                handle
                    .memory_reset(
                        codex_app_server_protocol::RequestId::String(format!(
                            "tui-memory-reset-{}",
                            Uuid::new_v4()
                        )),
                        None,
                    )
                    .await
            },
            |app, _, result| {
                match result {
                    Ok(_) => app
                        .chat_widget
                        .add_info_message("Reset local memories.".to_string(), None),
                    Err(error) => app
                        .chat_widget
                        .add_error_message(format!("Failed to reset memories: {error}")),
                }
                Ok(())
            },
        );
    }

    pub(super) fn submit_permission_runtime_update(
        &mut self,
        server: &AppServerSession,
        op: AppCommand,
    ) {
        let Some(thread_id) = self.active_thread_id else {
            self.chat_widget.submit_op(op);
            return;
        };
        let AppCommand::OverrideTurnContext {
            cwd,
            approval_policy,
            approvals_reviewer,
            active_permission_profile,
            model,
            effort,
            summary,
            service_tier,
            collaboration_mode,
            ..
        } = op
        else {
            unreachable!("feature settings require a turn-context override");
        };
        let params = ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            cwd,
            approval_policy,
            approvals_reviewer: approvals_reviewer
                .map(codex_app_server_protocol::ApprovalsReviewer::from),
            permissions: active_permission_profile.map(|profile| profile.id),
            model,
            effort: effort.unwrap_or_default(),
            summary,
            service_tier,
            collaboration_mode,
            ..Default::default()
        };
        self.enqueue_server_operation(
            server,
            thread_id,
            turn_submissions::ServerOperation::Mutation(
                turn_submissions::ThreadMutation::Settings(Box::new(params)),
            ),
        );
    }

    pub(super) fn queue_background_auto_review_settings(
        &mut self,
        server: &AppServerSession,
        origin: Option<ThreadId>,
        effective_config: &ConfigReadResponse,
        feature_updates: &[(Feature, bool)],
    ) {
        let Some(thread_id) = origin else {
            return;
        };
        if !feature_updates
            .iter()
            .any(|(feature, _)| *feature == Feature::GuardianApproval)
            || !feature_enabled_from_effective_config(effective_config, Feature::GuardianApproval)
            || sandbox_mode_from_effective_config(effective_config)
                != Some(AppServerSandboxMode::WorkspaceWrite)
        {
            return;
        }
        let preset = auto_review_mode();
        let params = ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            approval_policy: approval_policy_from_effective_config(effective_config),
            approvals_reviewer: approvals_reviewer_from_effective_config(effective_config)
                .map(codex_app_server_protocol::ApprovalsReviewer::from),
            permissions: Some(preset.active_permission_profile.id),
            ..Default::default()
        };
        self.enqueue_server_operation(
            server,
            thread_id,
            turn_submissions::ServerOperation::Mutation(
                turn_submissions::ThreadMutation::Settings(Box::new(params)),
            ),
        );
    }
}
