use super::*;
use crate::agent::control::ProviderAccountSelection;
use codex_login::anthropic::AccountId;
use codex_login::anthropic::AccountStore;
use std::sync::atomic::Ordering;
use tokio::sync::SemaphorePermit;

impl Session {
    /// Makes the selected account durable when the rollout first materializes, not when a
    /// blank composer opens. Acquires the persistence permit only while that binding is pending.
    pub(super) async fn bind_initial_account(
        &self,
        live_thread: &LiveThread,
    ) -> std::io::Result<()> {
        if self.initial_account_binding.initialized() || !self.binds_account() {
            return Ok(());
        }
        let settings = thread_settings::acquire_persistence_lock(self).await;
        self.bind_initial_account_under(&settings, live_thread)
            .await
    }

    /// `try_ensure_rollout_materialized` for a caller already holding the nonreentrant
    /// persistence permit.
    pub(super) async fn try_ensure_rollout_materialized_under(
        &self,
        settings: &SemaphorePermit<'_>,
        context: PersistContext,
    ) -> std::io::Result<()> {
        if let Some(live_thread) = self.live_thread() {
            self.bind_initial_account_under(settings, live_thread)
                .await?;
            live_thread
                .persist(context)
                .await
                .map_err(std::io::Error::other)?;
        }
        Ok(())
    }

    /// Every binder holds the permit, which serializes the snapshot with explicit account
    /// changes and leaves no initializer pending on a permit its caller holds.
    async fn bind_initial_account_under(
        &self,
        _settings: &SemaphorePermit<'_>,
        live_thread: &LiveThread,
    ) -> std::io::Result<()> {
        if !self.binds_account() {
            return Ok(());
        }
        self.initial_account_binding
            .get_or_try_init(|| async {
                live_thread
                    .append_items(&[RolloutItem::EventMsg(
                        thread_settings::applied_event(self).await,
                    )])
                    .await
                    .map_err(std::io::Error::other)?;
                live_thread.flush().await.map_err(std::io::Error::other)
            })
            .await?;
        Ok(())
    }

    fn binds_account(&self) -> bool {
        self.services.auth_manager.is_account_scoped()
            || self.services.model_client.provider_account_id().is_some()
    }

    pub(crate) fn committed_provider_account_id(&self) -> Option<String> {
        self.services.model_client.provider_account_id()
    }

    pub(crate) fn can_retry_after_account_handoff(&self, error: &CodexErr) -> bool {
        self.pending_account_id().is_some()
            && (error.retry_delay(/*retry_count*/ 1).is_some()
                || matches!(
                    error.details(),
                    CodexErrorDetails::RefreshTokenFailed(_)
                        | CodexErrorDetails::UsageLimitReached(_)
                        | CodexErrorDetails::QuotaExceeded
                        | CodexErrorDetails::OpenRouterKeyExhausted { .. }
                        | CodexErrorDetails::UsageNotIncluded
                        | CodexErrorDetails::ServerOverloaded
                ))
    }

    pub(crate) fn pending_account_id(&self) -> Option<String> {
        self.pending_account_selection()
            .map(|selection| selection.id().to_owned())
    }

    fn pending_account_selection(&self) -> Option<Arc<ProviderAccountSelection>> {
        if self.services.model_client.has_captured_provider() {
            return None;
        }
        self.services
            .local_agent_runtime
            .account_selection
            .get(&self.provider_id)
            .filter(|selection| match selection.as_ref() {
                ProviderAccountSelection::ClaudeSubscription { .. } => {
                    self.services.model_client.uses_claude_subscription()
                        && self
                            .services
                            .model_client
                            .provider_account_selection()
                            .as_ref()
                            != Some(selection.as_ref())
                }
                ProviderAccountSelection::OpenRouter { .. } => {
                    self.services.model_client.uses_openrouter()
                        && self
                            .services
                            .model_client
                            .provider_account_selection()
                            .as_ref()
                            != Some(selection.as_ref())
                }
                ProviderAccountSelection::Chatgpt { .. } => {
                    !self.services.model_client.uses_claude_subscription()
                        && !self.services.model_client.uses_openrouter()
                        && self.services.auth_manager.is_account_scoped()
                        && (self.account_refresh_pending.load(Ordering::Acquire)
                            || self.services.auth_manager.selected_account_id().as_deref()
                                != Some(selection.id()))
                }
            })
    }

    /// Accept a family-wide selection without changing credentials under running work.
    pub(crate) async fn select_account(&self, id: &str) -> std::io::Result<()> {
        if self.services.model_client.uses_claude_subscription()
            || self.services.model_client.uses_openrouter()
        {
            return Err(std::io::Error::other(
                "Select this conversation's saved provider account explicitly.",
            ));
        }
        if !self.services.auth_manager.is_account_scoped() {
            return Err(std::io::Error::other(
                "Account selection requires a conversation started with a saved ChatGPT login.",
            ));
        }
        Box::pin(
            self.publish_account_selection(ProviderAccountSelection::Chatgpt { id: id.to_owned() }),
        )
        .await
    }

    pub(crate) async fn select_claude_account(&self, id: &str) -> std::io::Result<()> {
        let account_id: AccountId = id.parse()?;
        let config = self.get_config().await;
        if !self.services.model_client.uses_claude_subscription()
            || !AccountStore::new(config.codex_home.to_path_buf())
                .list()?
                .iter()
                .any(|account| account.identity.id() == account_id)
        {
            return Err(std::io::Error::other(
                "Claude subscription account is not saved.",
            ));
        }
        Box::pin(
            self.publish_account_selection(ProviderAccountSelection::ClaudeSubscription {
                id: id.to_owned(),
            }),
        )
        .await
    }

    pub(crate) async fn select_openrouter_key(
        &self,
        key: &codex_protocol::KeyName,
    ) -> std::io::Result<()> {
        let config = self.get_config().await;
        if !self.services.model_client.uses_openrouter()
            || !codex_config::read_openrouter_config(&config.codex_home)
                .await?
                .key_files
                .contains_key(key)
        {
            return Err(std::io::Error::other("OpenRouter key is not saved."));
        }
        Box::pin(
            self.publish_account_selection(ProviderAccountSelection::OpenRouter {
                key: key.clone(),
            }),
        )
        .await
    }

    async fn publish_account_selection(
        &self,
        selection: ProviderAccountSelection,
    ) -> std::io::Result<()> {
        self.services.model_client.require_mutable_provider()?;
        let has_parent = self
            .state
            .lock()
            .await
            .session_configuration
            .parent_thread_id
            .is_some();
        if has_parent {
            let root = self
                .services
                .local_agent_runtime
                .root_thread()
                .await
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "Open the root conversation before selecting a family account: {error}"
                    ))
                })?;
            if root.session.thread_id() == self.thread_id() {
                return Err(std::io::Error::other(
                    "Family root is unavailable; open the owning root before selecting an account.",
                ));
            }
            if root.session.provider_id != self.provider_id
                || root.session.get_config().await.model_provider.adapter
                    != self.get_config().await.model_provider.adapter
                || root.session.services.auth_manager.is_account_scoped()
                    != self.services.auth_manager.is_account_scoped()
            {
                return Err(std::io::Error::other(
                    "Family account selection requires the same provider as the root conversation.",
                ));
            }
            if !Arc::ptr_eq(
                &root.session.services.local_agent_runtime.account_selection,
                &self.services.local_agent_runtime.account_selection,
            ) {
                return Err(std::io::Error::other(
                    "Family ownership changed; resume the child through its root before selecting an account.",
                ));
            }
            // Only the root may publish desired family ownership. Its durable
            // binding survives even when this caller's idle parent has no next turn.
            Box::pin(root.session.publish_account_selection(selection)).await?;
            return Box::pin(self.apply_pending_account_if_idle()).await;
        }
        let id = selection.id();
        // Validate before publishing. The refresh permit must not survive while waiting
        // for inference: that request may itself need to refresh its old credentials.
        {
            let _settings = thread_settings::acquire_persistence_lock(self).await;
            if matches!(&selection, ProviderAccountSelection::Chatgpt { .. }) {
                drop(self.services.auth_manager.prepare_account(id).await?);
            }
            let msg = self.persist_account_binding(id).await?;
            // Publish only after the root binding is durable. Restored children follow
            // this family selection rather than reviving their previous payer.
            self.services
                .local_agent_runtime
                .account_selection
                .select(&self.provider_id, selection);
            self.deliver_event_raw(Event {
                id: self.next_internal_event_id(),
                msg,
            })
            .await;
        }
        Box::pin(self.apply_pending_account_if_idle()).await
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "Reserve turn admission while applying idle-account changes."
    )]
    async fn apply_pending_account_if_idle(&self) -> std::io::Result<()> {
        if self.pending_account_id().is_none() {
            return Ok(());
        }
        let Ok(_work) = self.account_work.try_acquire() else {
            return Ok(());
        };
        // Settings updates acquire persistence before turn admission too.
        let _settings = thread_settings::acquire_persistence_lock(self).await;
        let active = self.active_turn.lock().await;
        if active.is_none() {
            Box::pin(self.commit_pending_account()).await?;
        }
        Ok(())
    }

    pub(crate) async fn settle_idle_account(&self) {
        if let Err(error) = Box::pin(self.apply_pending_account_if_idle()).await {
            self.send_event_raw(Event {
                id: self.next_internal_event_id(),
                msg: EventMsg::Warning(WarningEvent {
                    message: format!("Account switch remains pending: {error}"),
                }),
            })
            .await;
        }
    }

    /// Only the task driver (between steps) or an idle admission-lock owner may call this.
    pub(crate) async fn apply_pending_account(&self) -> std::io::Result<()> {
        if self.pending_account_id().is_none() {
            return Ok(());
        }
        let _settings = thread_settings::acquire_persistence_lock(self).await;
        Box::pin(self.commit_pending_account()).await
    }

    /// The caller owns the persistence permit and a safe work boundary.
    async fn commit_pending_account(&self) -> std::io::Result<()> {
        let Some(selection) = self.pending_account_selection() else {
            return Ok(());
        };
        let id = selection.id().to_owned();
        if matches!(
            selection.as_ref(),
            ProviderAccountSelection::ClaudeSubscription { .. }
                | ProviderAccountSelection::OpenRouter { .. }
        ) {
            let msg = self.persist_account_binding(&id).await?;
            self.services
                .model_client
                .commit_provider_account((*selection).clone());
            self.deliver_event_raw(Event {
                id: self.next_internal_event_id(),
                msg,
            })
            .await;
            return Ok(());
        }
        let prepared = self.services.auth_manager.prepare_account(&id).await?;
        let config = {
            let state = self.state.lock().await;
            self.build_effective_session_config(&state.session_configuration)
        };
        let msg = self.persist_account_binding(&id).await?;
        // Interruption after the credential commit must not strand old-account
        // capabilities. The next safe boundary retries the entire refresh.
        self.account_refresh_pending.store(true, Ordering::Release);
        // Commit has no await: cancellation cannot split the selector from the transport fence.
        prepared.commit();
        self.services.model_client.invalidate_account();
        self.services.models_manager.invalidate_account().await;
        self.services.plugins_manager.clear_cache();
        self.services
            .plugins_manager
            .clear_remote_installed_plugins_cache();
        // Account-dependent extension state uses the same rebuild path as configuration.
        for contributor in self.services.extensions.config_contributors() {
            contributor.on_config_changed(
                &self.services.session_extension_data,
                &self.services.thread_extension_data,
                &config,
                &config,
            );
        }
        self.request_mcp_runtime_refresh(McpRefreshReason::AccountWorkspaceSwitched);
        self.refresh_hooks(self.get_config().await).await;
        self.account_refresh_pending.store(false, Ordering::Release);
        self.deliver_event_raw(Event {
            id: self.next_internal_event_id(),
            msg,
        })
        .await;
        Ok(())
    }

    async fn persist_account_binding(&self, id: &str) -> std::io::Result<EventMsg> {
        let mut snapshot = self.thread_settings_snapshot().await;
        snapshot.account_id = Some(id.to_owned());
        let msg = EventMsg::ThreadSettingsApplied(ThreadSettingsAppliedEvent {
            thread_id: Some(self.thread_id()),
            thread_settings: snapshot,
        });
        if let Some(live) = self.live_thread() {
            live.append_items(&[RolloutItem::EventMsg(msg.clone())])
                .await
                .map_err(std::io::Error::other)?;
            live.flush().await.map_err(std::io::Error::other)?;
        }
        Ok(msg)
    }
}
