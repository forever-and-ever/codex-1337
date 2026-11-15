//! Resume and read-only history loading across rollout formats.

use super::AppServerSession;
use super::AppServerStartedThread;
use super::ResumeModelSettings;
use crate::legacy_core::config::Config;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_protocol::ThreadId;
use color_eyre::eyre::Result;

impl AppServerSession {
    pub(crate) async fn read_thread_for_viewing(
        &mut self,
        config: &Config,
        local_settings: &crate::local_settings::LocalSettings,
        thread_id: ThreadId,
    ) -> Result<(AppServerStartedThread, Option<&'static str>)> {
        let prepared = self
            .prepare_thread_view(
                super::session_preparation::RequestPolicy::Unbounded,
                config,
                local_settings,
                thread_id,
            )
            .run()
            .await;
        self.apply_session_observations(prepared.observations)?;
        prepared.result
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
        self.apply_session_observations(prepared.observations)?;
        prepared.result
    }
}

#[cfg(test)]
#[path = "rollout_history_tests.rs"]
mod tests;
