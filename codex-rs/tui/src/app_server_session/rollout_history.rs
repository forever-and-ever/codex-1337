//! Resume and read-only history loading across rollout formats.

use super::AppServerSession;
use super::AppServerStartedThread;
use super::HistoryHydrationScope;
use super::ResumeModelSettings;
use super::thread_session_state_from_thread_response;
use crate::legacy_core::config::Config;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::SortDirection;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadTurnsListParams;
use codex_app_server_protocol::ThreadTurnsListResponse;
use codex_app_server_protocol::TurnItemsView;
use codex_protocol::ThreadId;
use color_eyre::eyre::Result;

// Bound recovery to recent messages when item paging is unavailable.
const READ_ONLY_HISTORY_TURN_LIMIT: u32 = 100;

impl AppServerSession {
    /// Read a conflicting thread without taking its writer lease. This is a snapshot, not an
    /// attachment: the caller must not route turns through this session.
    pub(crate) async fn read_thread_for_viewing(
        &mut self,
        config: &Config,
        local_settings: &crate::local_settings::LocalSettings,
        thread_id: ThreadId,
    ) -> Result<(AppServerStartedThread, Option<&'static str>)> {
        let mut thread = self.thread_read(thread_id, /*include_turns*/ false).await?;
        let mut history_notice = None;
        if let Err(error) = self
            .hydrate_initial_thread_history(
                &mut thread,
                /*turn_cursor*/ None,
                /*item_cursor*/ None,
                Some(config),
                Some(local_settings),
                HistoryHydrationScope::Initial,
            )
            .await
        {
            if thread.history_mode == ThreadHistoryMode::Legacy {
                return Err(error);
            }
            // Summary pages retain user messages and final replies without scanning
            // every tool item when the item-paging endpoint is unavailable.
            tracing::warn!("Failed to page read-only thread history; trying summary pages");
            let request_id = self.next_request_id();
            let page: ThreadTurnsListResponse = self
                .client
                .request_typed(ClientRequest::ThreadTurnsList {
                    request_id,
                    params: ThreadTurnsListParams {
                        thread_id: thread_id.to_string(),
                        cursor: None,
                        limit: Some(READ_ONLY_HISTORY_TURN_LIMIT),
                        sort_direction: Some(SortDirection::Desc),
                        items_view: Some(TurnItemsView::Summary),
                    },
                })
                .await?;
            thread.turns = page.data.into_iter().rev().collect();
            self.history_pagination.remove(&thread_id);
            history_notice = Some(
                "Showing up to 100 recent prompts and final replies. Intermediate messages and tool activity are unavailable.",
            );
        }
        let session = thread_session_state_from_thread_response(
            &thread.id,
            crate::windows_sandbox::host_from_environments(thread.environments.as_deref()),
            thread.forked_from_id.clone(),
            thread.name.clone(),
            thread.path.clone(),
            thread.model.clone().unwrap_or_default(),
            thread.model_provider.clone(),
            config.service_tier.clone(),
            AskForApproval::from(config.permissions.approval_policy.value()),
            config.approvals_reviewer,
            config.permissions.permission_profile().clone(),
            config.permissions.active_permission_profile(),
            thread.cwd.clone(),
            config.workspace_roots.clone(),
            Vec::new(),
            thread.reasoning_effort,
            local_settings,
        )
        .await
        .map_err(color_eyre::eyre::Report::msg)?;
        Ok((
            AppServerStartedThread {
                can_accept_text_input: false,
                session,
                turns: thread.turns,
                blocks_direct_input: false,
                history_notice: None,
                task_tools_available: self.client.supports_thread_coordination(),
            },
            history_notice,
        ))
    }

    /// Retains trusted embedded-server history metadata for a later resume.
    ///
    /// Remote hints are ignored because another process can migrate the thread before resume.
    pub(crate) fn remember_thread_history_mode(
        &mut self,
        thread_id: ThreadId,
        history_mode: ThreadHistoryMode,
    ) {
        if !self.uses_embedded_app_server() {
            return;
        }
        self.history_pagination
            .entry(thread_id)
            .or_default()
            .history_mode = history_mode;
    }

    pub(crate) async fn resume_thread(
        &mut self,
        local_settings: &crate::local_settings::LocalSettings,
        config: Config,
        thread_id: ThreadId,
        model_settings: ResumeModelSettings,
    ) -> Result<AppServerStartedThread> {
        Box::pin(self.resume_thread_with_permission_overrides(
            local_settings,
            config,
            thread_id,
            model_settings,
            crate::resume_permissions::ResumePermissions::default(),
        ))
        .await
    }

    pub(crate) async fn resume_thread_with_permission_overrides(
        &mut self,
        local_settings: &crate::local_settings::LocalSettings,
        config: Config,
        thread_id: ThreadId,
        model_settings: ResumeModelSettings,
        permission_overrides: crate::resume_permissions::ResumePermissions,
    ) -> Result<AppServerStartedThread> {
        let prepared = self
            .prepare_resume_thread(
                super::session_preparation::RequestPolicy::Unbounded,
                local_settings,
                config,
                thread_id,
                model_settings,
                permission_overrides,
            )
            .run()
            .await;
        self.apply_session_observations(prepared.observations);
        prepared.result
    }
}

#[cfg(test)]
#[path = "rollout_history_tests.rs"]
mod tests;
