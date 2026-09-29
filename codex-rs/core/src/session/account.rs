use super::*;
use std::sync::atomic::Ordering;

impl Session {
    pub(crate) fn can_retry_after_account_handoff(&self, error: &CodexErr) -> bool {
        self.pending_account_id().is_some()
            && (error.retry_delay(/*retry_count*/ 1).is_some()
                || matches!(
                    error.details(),
                    CodexErrorDetails::RefreshTokenFailed(_)
                        | CodexErrorDetails::UsageLimitReached(_)
                        | CodexErrorDetails::QuotaExceeded
                        | CodexErrorDetails::UsageNotIncluded
                        | CodexErrorDetails::ServerOverloaded
                ))
    }

    pub(crate) fn pending_account_id(&self) -> Option<String> {
        if !self.services.auth_manager.is_account_scoped() {
            return None;
        }
        self.services
            .local_agent_runtime
            .account_selection
            .load_full()
            .filter(|id| {
                self.account_refresh_pending.load(Ordering::Acquire)
                    || self.services.auth_manager.selected_account_id().as_ref()
                        != Some(id.as_ref())
            })
            .map(|id| (*id).clone())
    }

    /// Accept a family-wide selection without changing credentials under running work.
    pub(crate) async fn select_account(&self, id: &str) -> std::io::Result<()> {
        if !self.services.auth_manager.is_account_scoped() {
            return Err(std::io::Error::other(
                "Account selection requires a conversation started with a saved ChatGPT login.",
            ));
        }
        // Validate before publishing. The refresh permit must not survive while waiting
        // for inference: that request may itself need to refresh its old credentials.
        {
            let _settings = thread_settings::acquire_persistence_lock(self).await;
            drop(self.services.auth_manager.prepare_account(id).await?);
            let msg = self.persist_account_binding(id).await?;
            // Publish only after the root binding is durable. Restored children follow
            // this family selection rather than reviving their previous payer.
            self.services
                .local_agent_runtime
                .account_selection
                .store(Some(Arc::new(id.to_owned())));
            self.deliver_event_raw(Event {
                id: self.next_internal_sub_id(),
                msg,
            })
            .await;
        }
        self.apply_pending_account_if_idle().await
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
            self.commit_pending_account().await?;
        }
        Ok(())
    }

    pub(crate) async fn settle_idle_account(&self) {
        if let Err(error) = self.apply_pending_account_if_idle().await {
            self.send_event_raw(Event {
                id: self.next_internal_sub_id(),
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
        self.commit_pending_account().await
    }

    /// The caller owns the persistence permit and a safe work boundary.
    async fn commit_pending_account(&self) -> std::io::Result<()> {
        let Some(id) = self.pending_account_id() else {
            return Ok(());
        };
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
        self.request_mcp_runtime_refresh();
        self.refresh_hooks(self.get_config().await).await;
        self.account_refresh_pending.store(false, Ordering::Release);
        self.deliver_event_raw(Event {
            id: self.next_internal_sub_id(),
            msg,
        })
        .await;
        Ok(())
    }

    async fn persist_account_binding(&self, id: &str) -> std::io::Result<EventMsg> {
        let mut snapshot = self.thread_settings_snapshot().await;
        snapshot.account_id = Some(id.to_owned());
        let msg =
            EventMsg::ThreadSettingsApplied(codex_protocol::protocol::ThreadSettingsAppliedEvent {
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
