use super::*;

impl ThreadManagerState {
    pub(super) async fn account_for_spawn(
        &self,
        config: &Config,
        history: &InitialHistory,
        parent: Option<ThreadId>,
        auth: &Arc<AuthManager>,
        agent_control: &LocalAgentControl,
    ) -> CodexResult<Option<Arc<AuthManager>>> {
        let owner = match history {
            InitialHistory::Resumed(history) => Some(history.conversation_id),
            InitialHistory::Forked(_) => history.forked_from_id(),
            InitialHistory::New | InitialHistory::Cleared => None,
        };
        let persisted = history
            .get_rollout_items()
            .iter()
            .rev()
            .find_map(|item| match item {
                RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event))
                    if owner.is_some() && event.thread_id == owner =>
                {
                    event.thread_settings.account_id.clone()
                }
                _ => None,
            });
        let inherited = if let Some(parent) = parent {
            // A resumed child can outlive its loaded parent. Its persisted
            // binding remains authoritative when the parent has been evicted.
            self.threads
                .read()
                .await
                .get(&parent)
                .map(|thread| thread.session.services.auth_manager.clone())
        } else {
            None
        };
        let source = inherited.as_ref().unwrap_or(auth);
        if persisted.is_none()
            && (source.has_external_auth()
                || source
                    .auth_cached()
                    .is_some_and(|auth| !auth.is_chatgpt_auth()))
        {
            return Ok(None);
        }
        let id = agent_control
            .account_selection
            .load_full()
            .map(|id| (*id).clone())
            .or(persisted)
            .or_else(|| {
                inherited
                    .as_ref()
                    .and_then(|auth| auth.selected_account_id())
            })
            .or_else(|| config.wrangler.default_account.clone())
            .or_else(|| source.selected_account_id());
        match id {
            Some(id) => source.for_account(&id).await.map(Some).map_err(|error| {
                CodexErr::InvalidRequest(format!("Cannot load conversation account: {error}"))
            }),
            None => Ok(None),
        }
    }
}
