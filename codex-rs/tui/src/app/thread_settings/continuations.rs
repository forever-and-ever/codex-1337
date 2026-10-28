//! Deferred settings updates and model-selection generations.

use super::App;
use super::AppEvent;
use super::AppServerApprovalsReviewer;
use super::AppServerAskForApproval;
use super::AppServerSession;
use super::ApprovalsReviewer;
use super::PermissionProfileSelection;
use super::ThreadId;
use super::ThreadSettingsUpdateParams;
use super::thread_settings_update_has_changes;
use crate::app_server_session::typed_requests::SessionRpc;

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
        self.request_thread_settings_update_with_outcome(server, params, move |app, result| {
            apply(app, matches!(result, Ok(true)));
        })
    }

    pub(in crate::app) fn request_thread_settings_update_with_outcome<A>(
        &mut self,
        server: &AppServerSession,
        params: ThreadSettingsUpdateParams,
        apply: A,
    ) -> bool
    where
        A: FnOnce(&mut App, Result<bool, &super::super::turn_submissions::SubmissionFailure>)
            + Send
            + 'static,
    {
        self.request_thread_settings_update_with_outcome_for_id(
            server,
            params,
            uuid::Uuid::new_v4(),
            None,
            apply,
        )
    }

    fn request_thread_settings_update_with_outcome_for_id<A>(
        &mut self,
        server: &AppServerSession,
        params: ThreadSettingsUpdateParams,
        request_id: uuid::Uuid,
        profile_selection: Option<PermissionProfileSelection>,
        apply: A,
    ) -> bool
    where
        A: FnOnce(&mut App, Result<bool, &super::super::turn_submissions::SubmissionFailure>)
            + Send
            + 'static,
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
        let reserves_profile = profile_selection.is_some();
        if let Some(selection) = profile_selection {
            self.pending_server_profiles.insert(
                thread_id,
                super::super::permission_reservations::PendingPermissionProfile {
                    request_id,
                    selection,
                },
            );
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
                        codex_app_server_protocol::RequestId::String(request_id.to_string()),
                        params,
                    )
                    .await
            },
            move |app, server, result| {
                let owns_reservation = app.owns_pending_permission_profile(thread_id, request_id);
                if matches!(result, Ok(false)) {
                    server.note_operation_settings_unsupported();
                }
                // Profile confirmation may precede this model receipt. Retired
                // permission authority cannot revoke the model's own acceptance.
                if changes_permissions
                    && (owns_reservation
                        || (!reserves_profile
                            && !app.pending_server_profiles.contains_key(&thread_id)))
                {
                    match result {
                        Ok(true) => {
                            app.agents_overview
                                .requested_permission_profiles
                                .remove(&thread_id);
                        }
                        Ok(false)
                        | Err(super::super::turn_submissions::SubmissionFailure::Rejected(_))
                            if owns_reservation =>
                        {
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
                apply(app, result);
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
    pub(in crate::app) fn request_active_model_settings<A>(
        &mut self,
        app_server: &AppServerSession,
        params: ThreadSettingsUpdateParams,
        apply: A,
    ) -> bool
    where
        A: FnOnce(&mut App, Result<bool, &super::super::turn_submissions::SubmissionFailure>)
            + Send
            + 'static,
    {
        let defaulted_to_auto_review = params.approvals_reviewer
            == Some(AppServerApprovalsReviewer::AutoReview)
            && (self.chat_widget.config_ref().approvals_reviewer != ApprovalsReviewer::AutoReview
                || AppServerAskForApproval::from(
                    self.chat_widget
                        .config_ref()
                        .permissions
                        .approval_policy
                        .value(),
                ) != AppServerAskForApproval::OnRequest);
        let pending_cyber_profile =
            params
                .permissions
                .as_ref()
                .map(|profile_id| PermissionProfileSelection {
                    profile_id: profile_id.clone(),
                    approval_policy: params.approval_policy,
                    approvals_reviewer: params
                        .approvals_reviewer
                        .map(codex_app_server_protocol::ApprovalsReviewer::to_core),
                    display_label: profile_id.clone(),
                });

        let thread_id = self.active_thread_id;
        let request_id = uuid::Uuid::new_v4();
        self.request_thread_settings_update_with_outcome_for_id(
            app_server,
            params,
            request_id,
            pending_cyber_profile,
            move |app, result| {
                if defaulted_to_auto_review
                    && matches!(result, Ok(true))
                    && app.active_thread_id == thread_id
                {
                    app.app_event_tx.send(AppEvent::CyberModelAutoReviewNotice);
                }
                apply(app, result);
            },
        )
    }

    pub(super) fn pending_automatic_model_setting(&self) -> Option<String> {
        self.chat_widget
            .automatic_model_switch_pending()
            .then(|| self.chat_widget.current_model().to_owned())
    }
}
