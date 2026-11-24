//! Runtime configuration persistence helpers for the TUI app.
//!
//! This module owns the app-level glue between config.toml edits, in-memory `Config` refreshes,
//! and the ChatWidget copy of session settings, keeping persistence-heavy code out of the main app
//! loop.

mod continuations;

use super::*;
use codex_config::ConfigLayerSource;

pub(super) async fn build_config_on_runtime_worker(
    builder: ConfigBuilder,
    application_network_policy: codex_http_client::NetworkPolicy,
    error_context: String,
) -> Result<Config> {
    continuations::build_config_on_runtime_worker_continuation(
        builder,
        application_network_policy,
        error_context,
    )
    .await
}

pub(super) fn resume_model_settings_for_overrides(
    config: &Config,
    harness_overrides: &ConfigOverrides,
) -> crate::app_server_session::ResumeModelSettings {
    let has_layer_override = config.config_layer_stack.layers_high_to_low().any(|layer| {
        matches!(
            &layer.name,
            ConfigLayerSource::SessionFlags
                | ConfigLayerSource::User {
                    profile: Some(_),
                    ..
                }
        ) && ["model", "model_provider", "model_reasoning_effort"]
            .iter()
            .any(|key| layer.config.get(*key).is_some())
    });
    if harness_overrides.model.is_some()
        || harness_overrides.model_provider.is_some()
        || has_layer_override
    {
        crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig
    } else {
        crate::app_server_session::ResumeModelSettings::RestoreFromThread
    }
}

pub(super) fn has_explicit_resume_permission_override(
    config: &Config,
    overrides: &ConfigOverrides,
) -> bool {
    // A directory choice is supported remotely even when permission overrides are not.
    let selected = crate::resume_permissions::ResumePermissions::from_overrides(
        config,
        &ConfigOverrides {
            cwd: None,
            ..overrides.clone()
        },
    );
    selected.approval_policy
        || selected.approvals_reviewer
        || selected.profile
        || selected.workspace_roots
}

impl App {
    pub(super) fn resume_permission_overrides(
        &self,
        config: &Config,
    ) -> crate::resume_permissions::ResumePermissions {
        let mut permission_overrides = crate::resume_permissions::ResumePermissions::from_overrides(
            config,
            &self.harness_overrides,
        );
        permission_overrides.approvals_reviewer |=
            self.runtime_approvals_reviewer_override.is_some();
        permission_overrides.approval_policy |= matches!(
            self.runtime_approval_policy_override,
            Some(RuntimeApprovalPolicyOverride::Explicit(_))
        );
        if let Some(profile) = self.runtime_permission_profile_override.as_ref()
            && profile.turn_override == RuntimePermissionProfileTurnOverride::LegacySandbox
        {
            permission_overrides.profile = true;
        }
        permission_overrides
    }

    pub(super) async fn rebuild_config_for_cwd(&self, cwd: PathBuf) -> Result<Config> {
        self.rebuild_config_for_cwd_continuation(cwd).await
    }

    #[cfg(test)]
    pub(super) async fn rebuild_config_for_permission_profile(
        &self,
        profile_id: &str,
    ) -> Result<Config> {
        let mut overrides = self.harness_overrides.clone();
        overrides.cwd = Some(self.chat_widget.config_ref().cwd.to_path_buf());
        overrides.sandbox_mode = None;
        overrides.permission_profile = None;
        overrides.default_permissions = Some(profile_id.to_string());
        let builder = ConfigBuilder::default()
            .codex_home(self.config.codex_home.to_path_buf())
            .cli_overrides(self.cli_kv_overrides.clone())
            .harness_overrides(overrides)
            .loader_overrides(self.loader_overrides.clone())
            .cloud_config_bundle(self.cloud_config_bundle.clone());
        build_config_on_runtime_worker(
            builder,
            self.config.application_network_policy.clone(),
            format!("Failed to rebuild config for permission profile {profile_id}"),
        )
        .await
    }

    // Local setup for startup fixtures; connected selections always use the server below.
    #[cfg(test)]
    pub(super) async fn apply_permission_profile_selection(
        &mut self,
        selection: PermissionProfileSelection,
    ) -> bool {
        if self.reject_pending_permission_change() {
            return false;
        }
        let PermissionProfileSelection {
            profile_id,
            approval_policy,
            approvals_reviewer,
            display_label,
        } = selection;
        let selected_config = match self
            .rebuild_config_for_permission_profile(profile_id.as_str())
            .await
        {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    profile_id,
                    "failed to resolve selected permission profile"
                );
                self.chat_widget.add_error_message(format!(
                    "Failed to set permission profile `{profile_id}`: {err}"
                ));
                return false;
            }
        };
        let permission_profile = selected_config.permissions.permission_profile();
        let active_permission_profile = selected_config.permissions.active_permission_profile();
        let network = selected_config.permissions.network.clone();

        let mut config = self.config.clone();
        if let Some(policy) = approval_policy
            && !self.try_set_approval_policy_on_config(
                &mut config,
                policy,
                "Failed to set approval policy",
                "failed to set selected permission profile approval policy on app config",
            )
        {
            return false;
        }
        if let Err(err) = config
            .permissions
            .set_permission_profile_from_session_snapshot(
                PermissionProfileSnapshot::from_session_snapshot(
                    permission_profile.clone(),
                    active_permission_profile.clone(),
                ),
            )
        {
            tracing::warn!(
                error = %err,
                profile_id,
                "failed to set selected permission profile on app config"
            );
            self.chat_widget.add_error_message(format!(
                "Failed to set permission profile `{profile_id}`: {err}"
            ));
            return false;
        }
        if let Some(reviewer) = approvals_reviewer {
            config.approvals_reviewer = reviewer;
        }
        config.permissions.network = network.clone();
        self.config = config;

        if let Some(policy) = approval_policy {
            self.runtime_approval_policy_override =
                Some(RuntimeApprovalPolicyOverride::Explicit(policy));
            self.chat_widget.set_approval_policy(policy);
        }
        if let Err(err) = self.chat_widget.set_permission_profile_with_active_profile(
            permission_profile.clone(),
            active_permission_profile.clone(),
        ) {
            tracing::warn!(
                error = %err,
                profile_id,
                "failed to set selected permission profile on chat config"
            );
            self.chat_widget.add_error_message(format!(
                "Failed to set permission profile `{profile_id}`: {err}"
            ));
            return false;
        }
        if let Some(reviewer) = approvals_reviewer {
            self.chat_widget.set_approvals_reviewer(reviewer);
        }
        self.chat_widget.set_permission_network(network);
        if let Some(thread_id) = self.chat_widget.thread_id() {
            self.agents_overview
                .selected_permission_profiles
                .insert(thread_id, profile_id.clone());
        }
        self.runtime_approvals_reviewer_override =
            approvals_reviewer.or(self.runtime_approvals_reviewer_override);
        self.runtime_permission_profile_override =
            Some(RuntimePermissionProfileOverride::from_config(&self.config));
        self.sync_active_thread_permission_settings_to_cached_session()
            .await;
        self.app_event_tx
            .send(AppEvent::CodexOp(AppCommand::override_turn_context(
                /*cwd*/ None,
                approval_policy,
                approvals_reviewer,
                Some(permission_profile.clone()),
                active_permission_profile,
                /*model*/ None,
                /*effort*/ None,
                /*summary*/ None,
                /*service_tier*/ None,
                /*collaboration_mode*/ None,
            )));
        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::new_info_event(
                format!("Permissions updated to {display_label}"),
                /*hint*/ None,
            ),
        )));
        true
    }

    pub(super) async fn select_permission_profile(
        &mut self,
        app_server: &mut AppServerSession,
        selection: PermissionProfileSelection,
    ) {
        if self.reject_pending_permission_change() {
            self.chat_widget
                .retain_input_after_failed_permission_selection();
            return;
        }
        let Some(thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget
                .retain_input_after_failed_permission_selection();
            self.chat_widget.add_error_message(
                "Wait for the task to connect before selecting permissions.".into(),
            );
            return;
        };
        let config = self.chat_widget.config_ref();
        let already_selected = !self
            .agents_overview
            .requested_permission_profiles
            .contains_key(&thread_id)
            && config
                .permissions
                .active_permission_profile()
                .is_some_and(|profile| profile.id == selection.profile_id)
            && selection.approval_policy.is_none_or(|policy| {
                config.permissions.approval_policy.value() == policy.to_core()
            })
            && selection
                .approvals_reviewer
                .is_none_or(|reviewer| config.approvals_reviewer == reviewer);
        if already_selected && selection.profile_id.starts_with(':') {
            self.agents_overview
                .selected_permission_profiles
                .insert(thread_id, selection.profile_id);
            self.chat_widget.submit_initial_user_message_if_pending();
            return;
        }
        let params = ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            permissions: Some(selection.profile_id.clone()),
            approval_policy: selection.approval_policy,
            approvals_reviewer: selection.approvals_reviewer.map(Into::into),
            ..Default::default()
        };
        self.select_permission_profile_continuation(
            app_server,
            selection,
            thread_id,
            params,
            config.permissions.approval_policy.value().into(),
            config.approvals_reviewer,
            already_selected,
        )
    }

    pub(super) fn reject_pending_permission_change(&mut self) -> bool {
        if self
            .chat_widget
            .thread_id()
            .is_some_and(|thread_id| self.pending_server_profiles.contains_key(&thread_id))
        {
            self.chat_widget.add_error_message(
                "Wait for permissions to update before changing permissions.".into(),
            );
            return true;
        }
        false
    }

    pub(super) fn reject_pending_permission_root_switch(&mut self) -> bool {
        if !self
            .pending_server_profiles
            .keys()
            .any(|thread_id| !self.thread_unavailable(*thread_id))
        {
            return false;
        }
        self.chat_widget
            .add_error_message("Wait for permissions to update before switching tasks.".into());
        true
    }

    pub(super) fn selected_server_profile(
        &self,
        thread_id: ThreadId,
    ) -> Option<PermissionProfileSelection> {
        if self.chat_widget.thread_id() != Some(thread_id) {
            return None;
        }
        if let Some(selection) = self
            .agents_overview
            .requested_permission_profiles
            .get(&thread_id)
        {
            return Some(selection.clone());
        }
        if self.app_server_target.thread_params_mode()
            == crate::app_server_session::ThreadParamsMode::Remote
        {
            return None;
        }
        let config = self.chat_widget.config_ref();
        let active = config.permissions.active_permission_profile()?;
        if active.id.starts_with(':')
            || (self
                .agents_overview
                .selected_permission_profiles
                .get(&thread_id)
                != Some(&active.id)
                && self
                    .config
                    .custom_permission_profiles
                    .iter()
                    .any(|profile| profile.id == active.id))
        {
            return None;
        }
        Some(PermissionProfileSelection {
            profile_id: active.id.clone(),
            approval_policy: Some(config.permissions.approval_policy.value().into()),
            approvals_reviewer: Some(config.approvals_reviewer),
            display_label: active.id,
        })
    }

    /// Adopt authoritative widget settings before deriving overrides for subsequent turns.
    pub(super) fn adopt_server_permissions(&mut self) {
        self.chat_widget.set_permission_network(/*network*/ None);
        self.config.permissions = self.chat_widget.config_ref().permissions.clone();
        self.config.approvals_reviewer = self.chat_widget.config_ref().approvals_reviewer;
        self.runtime_approval_policy_override = Some(RuntimeApprovalPolicyOverride::Explicit(
            self.config.permissions.approval_policy.value().into(),
        ));
        self.runtime_approvals_reviewer_override = if self
            .config
            .permissions
            .active_permission_profile()
            .is_some_and(|profile| profile.id.starts_with(':'))
        {
            Some(self.config.approvals_reviewer)
        } else {
            self.runtime_approvals_reviewer_override
                .filter(|reviewer| *reviewer == self.config.approvals_reviewer)
        };
        self.runtime_permission_profile_override =
            Some(RuntimePermissionProfileOverride::from_config(&self.config));
    }

    pub(super) fn adopt_inherited_server_selection(&mut self) {
        if let Some(thread_id) = self.chat_widget.thread_id()
            && let Some(active) = self
                .chat_widget
                .config_ref()
                .permissions
                .active_permission_profile()
        {
            self.agents_overview
                .selected_permission_profiles
                .insert(thread_id, active.id);
        }
        self.adopt_server_permissions();
    }

    pub(super) fn read_effective_config_after_overridden_write<A>(
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
        self.read_effective_config_after_overridden_write_continuation(server, setting, cwd, apply)
    }

    pub(super) fn apply_runtime_policy_overrides(
        &mut self,
        config: &mut Config,
        scope: RuntimePolicyOverrideScope,
    ) -> Result<()> {
        self.apply_runtime_policy_overrides_with_reviewer(
            config,
            scope,
            self.config.approvals_reviewer,
        )
    }

    pub(super) fn apply_runtime_policy_overrides_with_reviewer(
        &mut self,
        config: &mut Config,
        scope: RuntimePolicyOverrideScope,
        restored_reviewer: ApprovalsReviewer,
    ) -> Result<()> {
        if let Some(policy) = self.runtime_approval_policy_override
            && (scope == RuntimePolicyOverrideScope::All
                || matches!(policy, RuntimeApprovalPolicyOverride::Explicit(_)))
            && let Err(err) = config
                .permissions
                .approval_policy
                .set(policy.policy().to_core())
        {
            if scope == RuntimePolicyOverrideScope::ExplicitOnly {
                return Err(err).wrap_err("Failed to carry forward approval policy override");
            }
            tracing::warn!(%err, "failed to carry forward approval policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward approval policy override: {err}"
            ));
        }
        let profile_override =
            self.runtime_permission_profile_override
                .as_ref()
                .filter(|profile| {
                    scope == RuntimePolicyOverrideScope::All
                        || profile.turn_override
                            == RuntimePermissionProfileTurnOverride::LegacySandbox
                });
        if let Some(reviewer) = self.runtime_approvals_reviewer_override.or_else(|| {
            profile_override
                .filter(|profile| {
                    profile.turn_override == RuntimePermissionProfileTurnOverride::Preserve
                })
                .map(|_| restored_reviewer)
        }) {
            config
                .config_layer_stack
                .requirements()
                .approvals_reviewer
                .can_set(&reviewer)
                .wrap_err("Failed to carry forward approvals reviewer")?;
            config.approvals_reviewer = reviewer;
        }
        if let Some(profile_override) = profile_override {
            match config
                .permissions
                .set_permission_profile_from_session_snapshot(
                    PermissionProfileSnapshot::from_session_snapshot(
                        profile_override.permission_profile.clone(),
                        profile_override.active_permission_profile.clone(),
                    ),
                ) {
                Ok(()) => {
                    config.permissions.network = profile_override.network.clone();
                }
                Err(err) => {
                    if scope == RuntimePolicyOverrideScope::ExplicitOnly {
                        return Err(err)
                            .wrap_err("Failed to carry forward permission profile override");
                    }
                    tracing::warn!(%err, "failed to carry forward permission profile override");
                    self.chat_widget.add_error_message(format!(
                        "Failed to carry forward permission profile override: {err}"
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn set_approvals_reviewer_in_app_and_widget(&mut self, reviewer: ApprovalsReviewer) {
        self.config.approvals_reviewer = reviewer;
        self.chat_widget.set_approvals_reviewer(reviewer);
    }

    pub(super) fn try_set_approval_policy_on_config(
        &mut self,
        config: &mut Config,
        policy: AskForApproval,
        user_message_prefix: &str,
        log_message: &str,
    ) -> bool {
        if let Err(err) = config.permissions.approval_policy.set(policy.to_core()) {
            tracing::warn!(error = %err, "{log_message}");
            self.chat_widget
                .add_error_message(format!("{user_message_prefix}: {err}"));
            return false;
        }

        true
    }

    pub(super) fn try_set_builtin_active_permission_profile_on_config(
        &mut self,
        config: &mut Config,
        active_permission_profile: ActivePermissionProfile,
        user_message_prefix: &str,
        log_message: &str,
    ) -> Option<PermissionProfile> {
        let Some(permission_profile) =
            builtin_permission_profile_for_active_permission_profile(&active_permission_profile)
        else {
            tracing::warn!(
                id = %active_permission_profile.id,
                "{log_message}: unsupported active permission profile"
            );
            self.chat_widget.add_error_message(format!(
                "{user_message_prefix}: unsupported active permission profile `{}`",
                active_permission_profile.id
            ));
            return None;
        };

        if let Err(err) = config
            .permissions
            .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
                permission_profile.clone(),
                active_permission_profile,
            ))
        {
            tracing::warn!(error = %err, "{log_message}");
            self.chat_widget
                .add_error_message(format!("{user_message_prefix}: {err}"));
            return None;
        }

        Some(permission_profile)
    }

    pub(super) async fn update_feature_flags(
        &mut self,
        app_server: &mut AppServerSession,
        updates: Vec<(Feature, bool)>,
    ) {
        if updates.is_empty() {
            return;
        }
        if updates
            .iter()
            .any(|(feature, _)| *feature == Feature::GuardianApproval)
            && self.reject_pending_permission_change()
        {
            return;
        }

        let auto_review_preset = auto_review_mode();
        let mut next_config = self.config.clone();
        let mut approval_policy_override = None;
        let mut approvals_reviewer_override = None;
        let mut permission_profile_override = None;
        let mut active_permission_profile_override = None;
        let mut feature_updates_to_apply = Vec::with_capacity(updates.len());
        let mut permissions_history_label: Option<&'static str> = None;
        let mut config_edits = Vec::new();

        for (feature, enabled) in updates {
            let feature_key = feature.key();
            let mut feature_edits = Vec::new();
            let mut feature_config = next_config.clone();
            if let Err(err) = feature_config.features.set_enabled(feature, enabled) {
                tracing::error!(
                    error = %err,
                    feature = feature_key,
                    "failed to update constrained feature flags"
                );
                self.chat_widget.add_error_message(format!(
                    "Failed to update experimental feature `{feature_key}`: {err}"
                ));
                continue;
            }
            let effective_enabled = feature_config.features.enabled(feature);
            if feature == Feature::GuardianApproval {
                let previous_approvals_reviewer = feature_config.approvals_reviewer;
                if effective_enabled {
                    // Persist the reviewer setting so future sessions keep the
                    // experiment's matching `/permissions` mode until the user
                    // changes it explicitly.
                    feature_config.approvals_reviewer = auto_review_preset.approvals_reviewer;
                    feature_edits.push(crate::config_update::replace_config_value(
                        "approvals_reviewer",
                        serde_json::json!(auto_review_preset.approvals_reviewer.to_string()),
                    ));
                    if previous_approvals_reviewer != auto_review_preset.approvals_reviewer {
                        permissions_history_label = Some("Approve for me");
                    }
                } else if !effective_enabled {
                    feature_edits.push(crate::config_update::clear_config_value(
                        "approvals_reviewer",
                    ));
                    feature_config.approvals_reviewer = ApprovalsReviewer::User;
                    if previous_approvals_reviewer != ApprovalsReviewer::User {
                        permissions_history_label = Some("Ask for approval");
                    }
                }
                approvals_reviewer_override = Some(feature_config.approvals_reviewer);
            }
            if feature == Feature::GuardianApproval && effective_enabled {
                // The feature flag alone is not enough for the live session.
                // We also align approval policy + sandbox to the Auto-review
                // preset so enabling the experiment immediately
                // makes guardian review observable in the current thread.
                if !self.try_set_approval_policy_on_config(
                    &mut feature_config,
                    auto_review_preset.approval_policy,
                    "Failed to enable Approve for me",
                    "failed to set auto-review approval policy on staged config",
                ) {
                    continue;
                }
                let Some(permission_profile) = self
                    .try_set_builtin_active_permission_profile_on_config(
                        &mut feature_config,
                        auto_review_preset.active_permission_profile.clone(),
                        "Failed to enable Approve for me",
                        "failed to set auto-review permission profile on staged config",
                    )
                else {
                    continue;
                };
                feature_edits.extend([
                    crate::config_update::replace_config_value(
                        "approval_policy",
                        serde_json::json!("on-request"),
                    ),
                    crate::config_update::replace_config_value(
                        "sandbox_mode",
                        serde_json::json!("workspace-write"),
                    ),
                ]);
                approval_policy_override = Some(auto_review_preset.approval_policy);
                permission_profile_override = Some(permission_profile);
                active_permission_profile_override =
                    Some(auto_review_preset.active_permission_profile.clone());
            }
            next_config = feature_config;
            feature_updates_to_apply.push((feature, effective_enabled));
            config_edits.extend(feature_edits);
            config_edits.push(crate::config_update::build_feature_enabled_edit(
                feature_key,
                effective_enabled,
            ));
        }
        self.update_feature_flags_continuation(
            app_server,
            next_config,
            approval_policy_override,
            approvals_reviewer_override,
            permission_profile_override,
            active_permission_profile_override,
            feature_updates_to_apply,
            permissions_history_label,
            config_edits,
        )
    }

    pub(super) async fn update_memory_settings_with_app_server(
        &mut self,
        app_server: &mut AppServerSession,
        use_memories: bool,
        generate_memories: bool,
    ) {
        self.queue_memory_settings(app_server, use_memories, generate_memories);
    }

    pub(super) async fn reset_memories_with_app_server(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        self.reset_memories_with_app_server_continuation(app_server)
    }

    pub(super) fn reasoning_label(reasoning_effort: Option<&ReasoningEffortConfig>) -> String {
        match reasoning_effort {
            None | Some(ReasoningEffortConfig::None) => "default".to_string(),
            Some(reasoning_effort) => reasoning_effort.as_str().to_string(),
        }
    }

    pub(super) fn reasoning_label_for(
        model: &str,
        reasoning_effort: Option<&ReasoningEffortConfig>,
    ) -> Option<String> {
        (!model.starts_with("codex-auto-")).then(|| Self::reasoning_label(reasoning_effort))
    }

    pub(crate) fn token_usage(&self) -> crate::token_usage::TokenUsage {
        self.chat_widget.token_usage()
    }

    pub(super) fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        let clear_ephemeral_plan_effort = effort != Some(ReasoningEffortConfig::Ultra)
            && self.chat_widget.config_ref().plan_mode_reasoning_effort
                == Some(ReasoningEffortConfig::Ultra)
            && self.config.plan_mode_reasoning_effort != Some(ReasoningEffortConfig::Ultra);
        // TODO(aibrahim): Remove this and don't use config as a state object.
        // Instead, explicitly pass the stored collaboration mode's effort into new sessions.
        self.config.model_reasoning_effort = effort.clone();
        self.chat_widget.set_reasoning_effort(effort.clone());
        if clear_ephemeral_plan_effort {
            self.chat_widget.set_plan_mode_reasoning_effort(effort);
        }
    }

    pub(super) fn on_update_plan_mode_reasoning_effort(
        &mut self,
        effort: Option<ReasoningEffortConfig>,
    ) {
        let clear_ephemeral_default_effort = effort != Some(ReasoningEffortConfig::Ultra)
            && self
                .chat_widget
                .current_collaboration_mode()
                .reasoning_effort()
                == Some(ReasoningEffortConfig::Ultra)
            && self.config.model_reasoning_effort != Some(ReasoningEffortConfig::Ultra);
        self.config.plan_mode_reasoning_effort = effort.clone();
        self.chat_widget
            .set_plan_mode_reasoning_effort(effort.clone());
        if clear_ephemeral_default_effort {
            self.chat_widget.set_reasoning_effort(effort);
        }
    }

    pub(super) fn on_apply_advanced_reasoning(
        &mut self,
        model: &str,
        effort: ReasoningEffortConfig,
    ) -> Option<ReasoningEffortConfig> {
        let default_effort = self.default_reasoning_effort_for_conversation_model(model);
        if let Some(default_effort) = default_effort.as_ref() {
            self.config.model = Some(model.to_string());
            self.config.model_reasoning_effort = Some(default_effort.clone());
        }
        self.chat_widget.set_model(model);
        self.chat_widget.set_reasoning_effort(Some(effort.clone()));
        self.chat_widget
            .set_plan_mode_reasoning_effort(Some(effort));
        default_effort
    }

    fn default_reasoning_effort_for_conversation_model(
        &self,
        model: &str,
    ) -> Option<ReasoningEffortConfig> {
        let configured_effort = self
            .config
            .model_reasoning_effort
            .as_ref()
            .filter(|effort| **effort != ReasoningEffortConfig::Ultra);
        let preset = self
            .model_catalog
            .try_list_models()
            .ok()?
            .into_iter()
            .find(|preset| preset.model == model)?;
        let supported = &preset.supported_reasoning_efforts;

        configured_effort
            .filter(|effort| supported.iter().any(|option| option.effort == **effort))
            .cloned()
            .or_else(|| {
                (preset.default_reasoning_effort != ReasoningEffortConfig::Ultra)
                    .then_some(preset.default_reasoning_effort)
            })
            .or_else(|| {
                supported
                    .iter()
                    .find(|option| option.effort != ReasoningEffortConfig::Ultra)
                    .map(|option| option.effort.clone())
            })
    }

    pub(super) fn resume_model_settings(&self) -> crate::app_server_session::ResumeModelSettings {
        resume_model_settings_for_overrides(&self.config, &self.harness_overrides)
    }

    pub(super) fn reject_remote_resume_permission_override(
        &mut self,
        config: &Config,
        target: &AppServerTarget,
    ) -> bool {
        if target.thread_params_mode() != crate::app_server_session::ThreadParamsMode::Remote {
            return false;
        }
        let explicitly_selected =
            has_explicit_resume_permission_override(config, &self.harness_overrides)
                || self.runtime_approvals_reviewer_override.is_some()
                || matches!(
                    self.runtime_approval_policy_override,
                    Some(RuntimeApprovalPolicyOverride::Explicit(_))
                )
                || self
                    .runtime_permission_profile_override
                    .as_ref()
                    .is_some_and(|profile| {
                        profile.turn_override == RuntimePermissionProfileTurnOverride::LegacySandbox
                    });
        if explicitly_selected {
            self.add_agents_overview_error(
                "Permission overrides are not supported when resuming a remote task.".into(),
            );
            return true;
        }
        false
    }

    pub(super) fn restore_runtime_theme_from_config(&self) {
        self.resolve_runtime_theme();
    }

    fn sync_feature_state_from_effective_config(
        &mut self,
        effective_config: &ConfigReadResponse,
        feature_updates: &[(Feature, bool)],
    ) {
        for (feature, _) in feature_updates {
            let enabled = feature_enabled_from_effective_config(effective_config, *feature);
            if let Err(err) = self.config.features.set_enabled(*feature, enabled) {
                tracing::warn!(
                    error = %err,
                    feature = feature.key(),
                    "failed to sync effective feature state after an overridden write"
                );
                continue;
            }
            self.chat_widget.set_feature_enabled(*feature, enabled);
        }

        if feature_updates
            .iter()
            .any(|(feature, _)| *feature == Feature::GuardianApproval)
            && !self.config.features.enabled(Feature::GuardianApproval)
        {
            self.set_approvals_reviewer_in_app_and_widget(ApprovalsReviewer::User);
            return;
        }

        if let Some(reviewer) = approvals_reviewer_from_effective_config(effective_config) {
            self.set_approvals_reviewer_in_app_and_widget(reviewer);
        }
        if let Some(policy) = approval_policy_from_effective_config(effective_config) {
            if let Err(err) = self
                .config
                .permissions
                .approval_policy
                .set(policy.to_core())
            {
                tracing::warn!(
                    error = %err,
                    "failed to sync effective approval policy after an overridden write"
                );
                self.chat_widget.add_error_message(format!(
                    "Failed to refresh overridden Approve for me settings: {err}"
                ));
            } else {
                self.chat_widget.set_approval_policy(policy);
            }
        }
    }

    fn sync_auto_review_runtime_state_from_effective_config(
        &mut self,
        server: &AppServerSession,
        effective_config: &ConfigReadResponse,
        feature_updates: &[(Feature, bool)],
    ) {
        if !feature_updates
            .iter()
            .any(|(feature, _)| *feature == Feature::GuardianApproval)
            || !self.config.features.enabled(Feature::GuardianApproval)
            || sandbox_mode_from_effective_config(effective_config)
                != Some(AppServerSandboxMode::WorkspaceWrite)
        {
            return;
        }

        let auto_review_preset = auto_review_mode();
        let mut config = self.config.clone();
        let Some(permission_profile) = self.try_set_builtin_active_permission_profile_on_config(
            &mut config,
            auto_review_preset.active_permission_profile.clone(),
            "Failed to refresh overridden Approve for me settings",
            "failed to sync overridden Auto-review permission profile",
        ) else {
            return;
        };
        self.config = config;
        if let Err(err) = self
            .chat_widget
            .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
                permission_profile,
                auto_review_preset.active_permission_profile.clone(),
            ))
        {
            tracing::warn!(
                error = %err,
                "failed to sync overridden Auto-review permission profile on chat config"
            );
            self.chat_widget.add_error_message(format!(
                "Failed to refresh overridden Approve for me settings: {err}"
            ));
            return;
        }

        self.runtime_approvals_reviewer_override = Some(self.config.approvals_reviewer);
        self.runtime_permission_profile_override =
            Some(RuntimePermissionProfileOverride::from_config(&self.config));

        let approval_policy = AskForApproval::from(self.config.permissions.approval_policy.value());
        let op = AppCommand::override_turn_context(
            /*cwd*/ None,
            Some(approval_policy),
            Some(self.config.approvals_reviewer),
            /*permission_profile*/ None,
            Some(auto_review_preset.active_permission_profile),
            /*model*/ None,
            /*effort*/ None,
            /*summary*/ None,
            /*service_tier*/ None,
            /*collaboration_mode*/ None,
        );
        self.submit_permission_runtime_update(server, op);
    }

    fn sync_memory_state_from_effective_config(
        &mut self,
        effective_config: &ConfigReadResponse,
    ) -> bool {
        let Some(memories) = memories_from_effective_config(effective_config) else {
            tracing::warn!(
                "config/read omitted memories after an overridden memory settings write"
            );
            return false;
        };
        let use_memories = memories
            .use_memories
            .unwrap_or(self.config.memories.use_memories);
        let generate_memories = memories
            .generate_memories
            .unwrap_or(self.config.memories.generate_memories);
        self.config.memories.use_memories = use_memories;
        self.config.memories.generate_memories = generate_memories;
        self.chat_widget
            .set_memory_settings(use_memories, generate_memories);
        true
    }

    #[cfg(target_os = "windows")]
    pub(super) async fn verify_windows_sandbox_mode_after_setup(
        &mut self,
        app_server: &mut AppServerSession,
        requested_mode: codex_app_server_protocol::WindowsSandboxSetupMode,
    ) -> bool {
        if !self.refresh_windows_sandbox_config(app_server).await {
            return false;
        }
        if self.chat_widget.windows_sandbox_config.mode == Some(requested_mode)
            && self
                .chat_widget
                .windows_sandbox_config
                .allows(requested_mode)
        {
            return true;
        }
        self.chat_widget.add_error_message("Windows sandbox setup completed, but its mode was overridden by the effective configuration.".to_string());
        false
    }
}

pub(super) fn overridden_write_message(write_response: &ConfigWriteResponse) -> &str {
    write_response
        .overridden_metadata
        .as_ref()
        .map(|metadata| metadata.message.as_str())
        .unwrap_or("the effective config is overridden by a higher-priority layer")
}

fn feature_enabled_from_effective_config(
    effective_config: &ConfigReadResponse,
    feature: Feature,
) -> bool {
    let root_features = effective_config
        .config
        .additional
        .get("features")
        .and_then(features_toml_from_json);
    root_features
        .as_ref()
        .and_then(|features| features.entries().get(feature.key()).copied())
        .unwrap_or_else(|| feature.default_enabled())
}

fn approvals_reviewer_from_effective_config(
    effective_config: &ConfigReadResponse,
) -> Option<ApprovalsReviewer> {
    effective_config
        .config
        .approvals_reviewer
        .map(codex_app_server_protocol::ApprovalsReviewer::to_core)
}

fn approval_policy_from_effective_config(
    effective_config: &ConfigReadResponse,
) -> Option<AskForApproval> {
    effective_config.config.approval_policy
}

fn sandbox_mode_from_effective_config(
    effective_config: &ConfigReadResponse,
) -> Option<AppServerSandboxMode> {
    effective_config.config.sandbox_mode
}

fn memories_from_effective_config(effective_config: &ConfigReadResponse) -> Option<MemoriesToml> {
    effective_config
        .config
        .additional
        .get("memories")
        .and_then(|memories| serde_json::from_value(memories.clone()).ok())
}

fn features_toml_from_json(value: &serde_json::Value) -> Option<FeaturesToml> {
    serde_json::from_value(value.clone()).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::app::test_support::app_enabled_in_effective_config;
    use crate::app::test_support::make_test_app;
    use crate::legacy_core::config::edit::ConfigEdit;
    use crate::test_support::PathBufExt;
    use codex_config::ConfigLayerEntry;
    use codex_config::ConfigLayerStack;
    use codex_protocol::models::PermissionProfile;
    use codex_protocol::openai_models::ReasoningEffortPreset;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[tokio::test]
    async fn update_reasoning_effort_updates_collaboration_mode() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

        app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn conversation_reasoning_uses_compatible_default_for_new_threads() {
        for (configured_effort, expected_default_effort) in [
            (ReasoningEffortConfig::Low, ReasoningEffortConfig::Low),
            (
                ReasoningEffortConfig::Custom("unsupported".to_string()),
                ReasoningEffortConfig::Medium,
            ),
            (ReasoningEffortConfig::Ultra, ReasoningEffortConfig::Medium),
        ] {
            let mut app = make_test_app().await;
            app.config.model = Some("gpt-5.5".to_string());
            app.config.model_reasoning_effort = Some(configured_effort.clone());
            app.chat_widget
                .set_reasoning_effort(Some(configured_effort));

            let default_effort =
                app.on_apply_advanced_reasoning("gpt-5.5", ReasoningEffortConfig::Ultra);
            let new_thread_config = &app.config;

            assert_eq!(default_effort, Some(expected_default_effort.clone()));
            assert_eq!(app.chat_widget.current_model(), "gpt-5.5");
            assert_eq!(
                app.chat_widget.current_reasoning_effort(),
                Some(ReasoningEffortConfig::Ultra)
            );
            assert_eq!(
                (
                    new_thread_config.model.as_deref(),
                    new_thread_config.model_reasoning_effort.clone(),
                ),
                (Some("gpt-5.5"), Some(expected_default_effort))
            );
        }
    }

    #[tokio::test]
    async fn conversation_reasoning_keeps_previous_default_for_ultra_only_model() {
        let mut app = make_test_app().await;
        app.config.model = Some("gpt-5.5".to_string());
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Low);
        let mut preset = app
            .model_catalog
            .try_list_models()
            .expect("model catalog is infallible")
            .into_iter()
            .find(|preset| preset.model == "gpt-5.5")
            .expect("gpt-5.5 preset");
        preset.model = "ultra-only".to_string();
        preset.default_reasoning_effort = ReasoningEffortConfig::Ultra;
        preset.supported_reasoning_efforts = vec![ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Ultra,
            description: "Ultra reasoning".to_string(),
        }];
        app.model_catalog = Arc::new(ModelCatalog::new(vec![preset]));

        let default_effort =
            app.on_apply_advanced_reasoning("ultra-only", ReasoningEffortConfig::Ultra);
        let new_thread_config = &app.config;

        assert_eq!(default_effort, None);
        assert_eq!(app.chat_widget.current_model(), "ultra-only");
        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::Ultra)
        );
        assert_eq!(
            (
                new_thread_config.model.as_deref(),
                new_thread_config.model_reasoning_effort.clone(),
            ),
            (Some("gpt-5.5"), Some(ReasoningEffortConfig::Low))
        );
    }

    #[tokio::test]
    async fn conversation_reasoning_updates_active_plan_without_changing_plan_default() {
        let mut app = make_test_app().await;
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Low);
        app.config.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::High);
        app.chat_widget
            .set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
        app.chat_widget
            .set_plan_mode_reasoning_effort(Some(ReasoningEffortConfig::High));
        app.chat_widget
            .handle_key_event(KeyEvent::from(KeyCode::BackTab));

        let default_effort =
            app.on_apply_advanced_reasoning("gpt-5.5", ReasoningEffortConfig::Ultra);

        assert_eq!(default_effort, Some(ReasoningEffortConfig::Low));
        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::Ultra)
        );
        app.chat_widget
            .handle_key_event(KeyEvent::from(KeyCode::BackTab));
        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::Ultra)
        );
        app.chat_widget
            .handle_key_event(KeyEvent::from(KeyCode::BackTab));
        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::Ultra)
        );
        assert_eq!(
            (
                app.config.model_reasoning_effort.clone(),
                app.config.plan_mode_reasoning_effort.clone(),
            ),
            (
                Some(ReasoningEffortConfig::Low),
                Some(ReasoningEffortConfig::High),
            )
        );
    }

    #[tokio::test]
    async fn leaving_conversation_ultra_in_default_clears_the_ephemeral_plan_effort() {
        let mut app = make_test_app().await;
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Low);
        app.config.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::High);
        app.chat_widget
            .set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
        app.chat_widget
            .set_plan_mode_reasoning_effort(Some(ReasoningEffortConfig::High));

        app.on_apply_advanced_reasoning("gpt-5.4", ReasoningEffortConfig::Ultra);
        app.on_update_reasoning_effort(Some(ReasoningEffortConfig::Medium));
        app.chat_widget
            .handle_key_event(KeyEvent::from(KeyCode::BackTab));

        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::Medium)
        );
        assert_eq!(
            app.config.plan_mode_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn leaving_conversation_ultra_in_plan_clears_the_ephemeral_default_effort() {
        let mut app = make_test_app().await;
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Low);
        app.config.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::High);
        app.chat_widget
            .set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
        app.chat_widget
            .set_plan_mode_reasoning_effort(Some(ReasoningEffortConfig::High));
        app.chat_widget
            .handle_key_event(KeyEvent::from(KeyCode::BackTab));

        app.on_apply_advanced_reasoning("gpt-5.4", ReasoningEffortConfig::Ultra);
        app.on_update_plan_mode_reasoning_effort(Some(ReasoningEffortConfig::Medium));
        app.chat_widget
            .handle_key_event(KeyEvent::from(KeyCode::BackTab));

        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::Medium)
        );
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::Low)
        );
    }

    #[tokio::test]
    async fn resume_model_settings_preserves_only_explicit_model_overrides() {
        let mut app = make_test_app().await;

        assert_eq!(
            app.resume_model_settings(),
            crate::app_server_session::ResumeModelSettings::RestoreFromThread
        );
        let profile_path = test_path_buf("/tmp/work.config.toml").abs();
        let profile = "work"
            .parse::<codex_config::ProfileV2Name>()
            .expect("valid profile name");
        for (key, expected) in [
            (
                "model",
                crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig,
            ),
            (
                "model_provider",
                crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig,
            ),
            (
                "model_reasoning_effort",
                crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig,
            ),
            (
                "sandbox_mode",
                crate::app_server_session::ResumeModelSettings::RestoreFromThread,
            ),
        ] {
            let config = TomlValue::Table(toml::map::Map::from_iter([(
                key.to_string(),
                TomlValue::String("value".to_string()),
            )]));
            app.config.config_layer_stack = ConfigLayerStack::new(
                vec![ConfigLayerEntry::new(
                    ConfigLayerSource::SessionFlags,
                    config.clone(),
                )],
                Default::default(),
                Default::default(),
            )
            .expect("session flags layer stack");
            assert_eq!(app.resume_model_settings(), expected);

            app.config.config_layer_stack = ConfigLayerStack::default()
                .with_user_config_profile(&profile_path, Some(&profile), config)
                .expect("user config profile layer stack");
            assert_eq!(app.resume_model_settings(), expected);
        }

        app.config.config_layer_stack = ConfigLayerStack::default()
            .with_user_config(
                &profile_path,
                TomlValue::Table(toml::map::Map::from_iter([(
                    "model_reasoning_effort".to_string(),
                    TomlValue::String("high".to_string()),
                )])),
            )
            .expect("user config layer stack");
        assert_eq!(
            app.resume_model_settings(),
            crate::app_server_session::ResumeModelSettings::RestoreFromThread
        );

        app.harness_overrides.model_provider = Some("custom-provider".to_string());
        assert_eq!(
            app.resume_model_settings(),
            crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig
        );
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_loads_latest_apps_state() -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        let app_id = "unit_test_refresh_in_memory_config_connector".to_string();

        assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

        ConfigEditsBuilder::for_config(&app.config)
            .with_edits([
                ConfigEdit::SetPath {
                    segments: vec!["apps".to_string(), app_id.clone(), "enabled".to_string()],
                    value: false.into(),
                },
                ConfigEdit::SetPath {
                    segments: vec![
                        "apps".to_string(),
                        app_id.clone(),
                        "disabled_reason".to_string(),
                    ],
                    value: "user".into(),
                },
            ])
            .apply()
            .await
            .expect("persist app toggle");

        assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

        app.refresh_test_config().await?;

        assert_eq!(
            app_enabled_in_effective_config(&app.config, &app_id),
            Some(false)
        );
        Ok(())
    }

    // Regression coverage for `/new` and `/clear`: cloud requirements
    // must survive the config refresh that runs before thread transitions.
    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_keeps_cloud_requirements_for_thread_transitions()
    -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        let required_policy = codex_protocol::protocol::AskForApproval::Never;
        let cloud_config_bundle =
            codex_config::test_support::CloudConfigBundleFixture::loader_with_enterprise_requirement(
                r#"allowed_approval_policies = ["never"]"#,
            );

        let config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .cloud_config_bundle(cloud_config_bundle.clone())
            .build()
            .await?;
        app.config = config;
        app.cloud_config_bundle = cloud_config_bundle;
        let app_id = "unit_test_cloud_requirements_reload_marker";
        std::fs::write(
            codex_home.path().join("config.toml"),
            format!(
                r#"
[apps.{app_id}]
enabled = false
"#
            ),
        )?;

        let assert_cloud_requirements = |app: &App| {
            let config = &app.config;
            assert_eq!(
                config
                    .config_layer_stack
                    .requirements_toml()
                    .allowed_approval_policies
                    .clone(),
                Some(vec![required_policy])
            );
            assert_eq!(config.permissions.approval_policy.value(), required_policy);
        };

        assert_cloud_requirements(&app);
        assert_eq!(app_enabled_in_effective_config(&app.config, app_id), None);

        // This is the fallible reload that the best-effort `/new`, `/clear`,
        // `/fork`, side-conversation, and session-picker paths wrap.
        app.refresh_test_config().await?;

        assert_eq!(
            app_enabled_in_effective_config(&app.config, app_id),
            Some(false)
        );
        assert_cloud_requirements(&app);
        Ok(())
    }

    #[tokio::test]
    async fn new_session_and_config_rebuilds_keep_the_live_application_network_policy() -> Result<()>
    {
        use codex_http_client::ClientRouteClass;
        use codex_http_client::DestinationPolicy;
        use codex_http_client::HttpError;
        use codex_http_client::NetworkPolicyController;
        use codex_http_client::NetworkPolicyDenied;
        use wiremock::Mock;
        use wiremock::MockServer;
        use wiremock::ResponseTemplate;
        use wiremock::matchers::method;

        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        let controller = NetworkPolicyController::default();
        let denied = DestinationPolicy::Restricted {
            allowed_hosts: Default::default(),
        };
        assert!(controller.publish(controller.policy().revision(), denied.clone()));
        app.config.application_network_policy = controller.policy();
        let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        app.refresh_test_config().await?;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.start_fresh_session_and_settle(&mut tui, &mut app_server, None, None, None)
            .await;
        let new_config = app.config.clone();
        let permission_config = app
            .rebuild_config_for_permission_profile(":workspace")
            .await?;
        let denied_url = "https://example.com/asset".parse()?;
        for config in [&app.config, &new_config, &permission_config] {
            assert_eq!(config.application_network_policy, controller.policy());
            assert_eq!(
                config
                    .http_client_factory()
                    .network_policy()
                    .acquire(&denied_url)
                    .map(|_| ()),
                Err(NetworkPolicyDenied::Destination),
            );
        }
        let client = new_config
            .http_client_factory()
            .build_client(&server.uri(), ClientRouteClass::Other)?;
        assert!(matches!(
            client.get(server.uri()).send().await,
            Err(HttpError::Policy(NetworkPolicyDenied::Destination))
        ));
        assert!(server.received_requests().await.unwrap().is_empty());

        assert!(controller.publish(
            controller.policy().revision(),
            DestinationPolicy::Unrestricted
        ));
        assert!(client.get(server.uri()).send().await?.status().is_success());
        assert!(controller.publish(controller.policy().revision(), denied));
        assert!(matches!(
            client.get(server.uri()).send().await,
            Err(HttpError::Policy(NetworkPolicyDenied::Destination))
        ));
        server.verify().await;
        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_best_effort_keeps_current_config_on_error()
    -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
        let original_config = app.config.clone();

        let result = app
            .capture_config_rebuild(app.chat_widget.config_ref().cwd.to_path_buf(), None)
            .run()
            .await;
        app.apply_navigation_config_refresh(result, "starting a new thread");

        assert_eq!(app.config, original_config);
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_uses_active_chat_widget_cwd() -> Result<()> {
        let mut app = make_test_app().await;
        let original_cwd = app.config.cwd.clone();
        let next_cwd_tmp = tempdir()?;
        let next_cwd = next_cwd_tmp.path().to_path_buf();

        app.chat_widget
            .handle_thread_session(crate::session_state::ThreadSessionState {
                read_only_history: false,
                thread_settings_confirmed: true,
                primary_environment_cwd: None,
                daybreak_enabled: false,
                windows_sandbox_host: crate::app::WindowsSandboxHost::Local,
                thread_id: ThreadId::new(),
                forked_from_id: None,
                fork_parent_title: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                permission_profile: PermissionProfile::read_only(),
                active_permission_profile: None,
                cwd: next_cwd.clone().abs().into(),
                runtime_workspace_roots: Vec::new(),
                instruction_source_paths: Vec::new(),
                reasoning_effort: None,
                collaboration_mode: None,
                message_history: None,
                network_proxy: None,
                rollout_path: Some(PathBuf::new()),
            });

        assert_eq!(app.chat_widget.config_ref().cwd.to_path_buf(), next_cwd);
        assert_eq!(app.config.cwd, original_cwd);

        app.refresh_test_config().await?;

        assert_eq!(app.config.cwd, app.chat_widget.config_ref().cwd);
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_updates_live_widget_preferences() -> Result<()> {
        let mut app = make_test_app().await;
        let mut expected_widget_settings = app.chat_widget.local_settings.clone();
        expected_widget_settings.tui.terminal_resize_reflow_max_rows = Some(9000);
        expected_widget_settings.tui.session_picker_view =
            Some(codex_config::types::SessionPickerViewMode::Comfortable);
        expected_widget_settings.tui.resume_cwd = Some(codex_config::types::ResumeCwdMode::Current);
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        std::fs::write(
            codex_home.path().join("config.toml"),
            r#"
[tui]
terminal_resize_reflow_max_rows = 9000
session_picker_view = "comfortable"
resume_cwd = "current"
theme = "dracula"
"#,
        )?;

        app.refresh_test_config().await?;

        assert_eq!(
            app.local_settings.terminal_resize_reflow().max_rows,
            crate::legacy_core::config::TerminalResizeReflowMaxRows::Limit(9000)
        );
        assert_eq!(
            app.local_settings.tui.session_picker_view,
            Some(codex_config::types::SessionPickerViewMode::Comfortable)
        );
        assert_eq!(app.local_settings.tui.theme.as_deref(), Some("dracula"));
        assert_eq!(app.chat_widget.local_settings, expected_widget_settings);
        Ok(())
    }

    #[tokio::test]
    async fn overridden_disabled_guardian_does_not_apply_auto_review_companions() -> Result<()> {
        let mut app = make_test_app().await;
        let original_policy = app.config.permissions.approval_policy.value();
        let effective_config: ConfigReadResponse = serde_json::from_value(serde_json::json!({
            "config": {
                "approval_policy": AskForApproval::OnRequest,
                "approvals_reviewer": codex_app_server_protocol::ApprovalsReviewer::AutoReview,
                "sandbox_mode": AppServerSandboxMode::WorkspaceWrite,
                "features": {
                    "guardian_approval": false,
                },
            },
            "origins": {},
        }))?;

        app.sync_feature_state_from_effective_config(
            &effective_config,
            &[(Feature::GuardianApproval, /*enabled*/ true)],
        );

        assert!(!app.config.features.enabled(Feature::GuardianApproval));
        assert!(
            !app.chat_widget
                .config_ref()
                .features
                .enabled(Feature::GuardianApproval)
        );
        assert_eq!(app.config.approvals_reviewer, ApprovalsReviewer::User);
        assert_eq!(
            app.chat_widget.config_ref().approvals_reviewer,
            ApprovalsReviewer::User
        );
        assert_eq!(
            app.config.permissions.approval_policy.value(),
            original_policy
        );
        Ok(())
    }

    #[tokio::test]
    async fn new_thread_keeps_live_settings_after_failed_reload() -> Result<()> {
        let mut app = make_test_app().await;
        let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let home = tempdir()?;
        app.config.codex_home = home.path().to_path_buf().abs();
        let config_path = home.path().join("config.toml");
        std::fs::write(&config_path, "[tui]\ntheme = 'dracula'\n")?;
        app.refresh_test_config().await?;
        // A live setting can be newer than the last successful disk load.
        app.local_settings.tui.rendering.math = false;
        std::fs::write(config_path, "[broken")?;

        let expected = app.local_settings.clone();
        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.start_fresh_session_and_settle(&mut tui, &mut server, None, None, None)
            .await;
        assert_eq!(app.local_settings, expected);
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_config_for_resume_or_fallback_uses_current_config_on_same_cwd_error()
    -> Result<()> {
        let mut app = make_test_app().await;
        app.local_settings.tui.theme = Some("dracula".to_string());
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
        let current_config = app.config.clone();
        let current_cwd = current_config.cwd.clone();

        let resume_config = app
            .capture_config_rebuild(current_cwd.to_path_buf(), Some(&current_cwd))
            .run()
            .await?;

        assert_eq!(*resume_config, (current_config, app.local_settings.clone()));
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_config_for_resume_or_fallback_errors_when_cwd_changes() -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
        let current_cwd = app.config.cwd.clone();
        let next_cwd_tmp = tempdir()?;
        let next_cwd = next_cwd_tmp.path().to_path_buf();

        let result = app
            .capture_config_rebuild(next_cwd, Some(&current_cwd))
            .run()
            .await;

        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn config_refresh_repaints_conversations_without_an_override() {
        let mut app = make_test_app().await;
        for name in ["slag", "eclipse", "random"] {
            app.local_settings.tui.theme = Some(name.to_string());
            app.restore_runtime_theme_from_config();
            assert_eq!(
                crate::render::highlight::current_syntax_theme()
                    .name
                    .as_deref(),
                Some(if name == "random" {
                    crate::warpaint::DEFAULT_PACK
                } else {
                    name
                })
            );
        }
    }

    #[tokio::test]
    async fn replacement_preserves_live_local_settings_and_server_auth_requirement() -> Result<()> {
        let mut app = make_test_app().await;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.local_settings.tui.theme = Some("dracula".to_string());
        app.chat_widget.test_account_mut().requires_openai_auth = false;
        crate::markdown_render::preferences::init(Default::default());
        app.local_settings.tui.rendering.math = false;
        let mut legacy_config = app.config.clone();
        legacy_config.tui_theme = Some("nord".to_string());
        legacy_config.model_provider.requires_openai_auth = true;
        let init = app.chatwidget_init_for_forked_or_resumed_thread(
            &mut tui,
            legacy_config,
            /*initial_user_message*/ None,
        );
        let replacement = ChatWidget::new_with_app_event(init);
        assert_eq!(replacement.local_settings, app.local_settings);
        assert!(!replacement.requires_openai_auth());
        app.replace_chat_widget(replacement);
        let source = r"Math: \(x^2\)";
        assert_eq!(
            crate::markdown_render::render_markdown_text(source).to_string(),
            source
        );
        Ok(())
    }
}
