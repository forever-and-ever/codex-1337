//! Deferred settings updates and model-selection generations.

use super::*;
use crate::app_server_session::typed_requests::AppServerRequestHandleExt;

impl App {
    pub(in crate::app) fn request_thread_settings_update<A>(
        &mut self,
        server: &AppServerSession,
        params: ThreadSettingsUpdateParams,
        apply: A,
    ) -> bool
    where
        A: FnOnce(&mut App, bool) + Send + 'static,
    {
        if !thread_settings_update_has_changes(&params) || !server.operation_settings_supported() {
            return false;
        }
        let Ok(thread_id) = ThreadId::from_string(&params.thread_id) else {
            return false;
        };
        let changes_permissions = params.cwd.is_some()
            || params.approval_policy.is_some()
            || params.approvals_reviewer.is_some()
            || params.sandbox_policy.is_some()
            || params.permissions.is_some();
        if changes_permissions && self.pending_server_profiles.contains_key(&thread_id) {
            return false;
        }
        let generation = self.model_selection_generation;
        self.request_server(
            server,
            super::super::turn_submissions::ui_requests::UiRequestScope::Thread(thread_id),
            super::super::turn_submissions::ui_requests::UiRequestKind::Mutation,
            "Updating thread settings",
            move |handle| async move {
                handle
                    .thread_settings_update(
                        codex_app_server_protocol::RequestId::String(
                            uuid::Uuid::new_v4().to_string(),
                        ),
                        params,
                    )
                    .await
            },
            move |app, server, result| {
                if matches!(result, Ok(false)) {
                    server.note_operation_settings_unsupported();
                }
                if changes_permissions {
                    match result {
                        Ok(true) => {
                            app.agents_overview
                                .requested_permission_profiles
                                .remove(&thread_id);
                        }
                        Ok(false)
                        | Err(super::super::turn_submissions::SubmissionFailure::Rejected(_)) => {
                            app.pending_server_profiles.remove(&thread_id);
                        }
                        _ => {}
                    }
                }
                if let Err(error) = result {
                    tracing::warn!(%error, "failed to update app-server thread settings from TUI");
                    app.chat_widget
                        .add_error_message(format!("Failed to update thread settings: {error}"));
                }
                apply(app, matches!(result, Ok(true)));
                if matches!(result, Ok(true))
                    && app.model_selection_generation == generation
                    && app.chat_widget.thread_id() == Some(thread_id)
                    && app.active_thread_id == Some(thread_id)
                {
                    app.chat_widget.finish_automatic_model_switch();
                    if !app.rate_limit_refresh_state.has_pending_recovery() {
                        app.chat_widget.finish_rate_limit_recovery();
                    }
                }
                Ok(())
            },
        );
        true
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct ModelSelectionGeneration(u64);

impl ModelSelectionGeneration {
    #[expect(
        clippy::expect_used,
        reason = "exhausting the model-selection generation is an invariant failure"
    )]
    pub(in crate::app) fn advance(&mut self) {
        self.0 = self
            .0
            .checked_add(1)
            .expect("model selection generation exhausted");
    }
}

impl App {
    pub(super) fn request_active_model_settings(
        &mut self,
        app_server: &AppServerSession,
        params: ThreadSettingsUpdateParams,
        defaulted_to_auto_review: bool,
        pending_cyber_profile: Option<PermissionProfileSelection>,
    ) {
        let thread_id = self.active_thread_id;
        let accepted =
            self.request_thread_settings_update(app_server, params, move |app, updated| {
                if defaulted_to_auto_review && updated && app.active_thread_id == thread_id {
                    app.app_event_tx.send(AppEvent::CyberModelAutoReviewNotice);
                }
            });
        // Reserve the selection before its confirming notification can arrive.
        if accepted && let (Some(thread_id), Some(selection)) = (thread_id, pending_cyber_profile) {
            self.pending_server_profiles.insert(thread_id, selection);
        }
    }

    pub(super) fn pending_automatic_model_setting(&self) -> Option<String> {
        self.chat_widget
            .automatic_model_switch_pending()
            .then(|| self.chat_widget.current_model().to_owned())
    }
}
