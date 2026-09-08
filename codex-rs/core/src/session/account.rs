use super::*;

impl Session {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "Turn admission stays reserved until the persisted account and runtime credentials agree."
    )]
    pub(crate) async fn select_account(&self, id: &str) -> std::io::Result<()> {
        if !self.services.auth_manager.is_account_scoped() {
            return Err(std::io::Error::other(
                "Account selection requires a conversation started with a saved ChatGPT login.",
            ));
        }
        // This is the same admission lock that reserves a new turn. No UI-state guess
        // or queued Op may move credentials underneath running inference or a tool call.
        let active = self.active_turn.lock().await;
        if active.is_some() {
            return Err(std::io::Error::other(
                "Stop the current turn before changing accounts.",
            ));
        }
        let _settings = thread_settings::acquire_persistence_lock(self).await;
        let prepared = self.services.auth_manager.prepare_account(id).await?;
        let mut snapshot = self.thread_settings_snapshot().await;
        let config = {
            let state = self.state.lock().await;
            self.build_effective_session_config(&state.session_configuration)
        };
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
        self.deliver_event_raw(Event {
            id: self.next_internal_sub_id(),
            msg,
        })
        .await;
        Ok(())
    }
}
