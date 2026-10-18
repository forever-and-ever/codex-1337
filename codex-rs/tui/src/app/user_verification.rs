//! Coordinates approval through the local app-server, keeping proofs out of views.
//!
//! Attempt IDs are UI-local generations: cancellation, resolution, or reconnect removes the
//! pending generation, so a late RPC response cannot approve a newly surfaced request.

use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::UserVerificationProof;
use codex_app_server_protocol::UserVerificationVerifyResponse;
use codex_app_server_protocol::WarningNotification;
use codex_protocol::ThreadId;
use uuid::Uuid;

use super::App;
use super::app_server_requests::ResolvedAppServerRequest;
use super::thread_delivery::LocalThreadCompletion;
use super::user_verification_errors::verification_failure;
use crate::app_command::AppCommand;
use crate::app_command::UserVerificationFailure;
use crate::app_command::UserVerificationResponse;
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol::UserVerificationErrorDetails;
use codex_app_server_protocol::UserVerificationUnavailableReason;

impl App {
    pub(super) async fn start_user_verification(
        &mut self,
        app_server: &AppServerSession,
        thread_id: ThreadId,
        server_name: String,
        request_id: RequestId,
    ) -> color_eyre::Result<()> {
        let Some(attempt) = self.pending_app_server_requests.user_verification.begin(
            &thread_id.to_string(),
            &server_name,
            &request_id,
        ) else {
            return Ok(());
        };
        let handle = app_server.request_handle();
        let completion_tx = self.thread_delivery.completion_tx.clone();
        let remote_workspace = app_server.uses_remote_workspace();
        tokio::spawn(async move {
            let attempt_id = attempt.id;
            let verification = async {
                if remote_workspace {
                    return Err(UserVerificationFailure {
                        message: "User verification is unavailable for remote workspaces.",
                        details: UserVerificationErrorDetails::Unavailable {
                            reason: UserVerificationUnavailableReason::ProviderUnavailable,
                        },
                    });
                }
                handle
                    .request_typed::<UserVerificationVerifyResponse>(
                        ClientRequest::UserVerificationVerify {
                            request_id: RequestId::String(format!(
                                "user-verification-{attempt_id}"
                            )),
                            params: attempt.params,
                        },
                    )
                    .await
                    .map(|response| response.proof)
                    .map_err(|error| verification_failure(&error))
            };
            let result = tokio::select! {
                biased;
                _ = attempt.cancelled.cancelled() => return,
                result = verification => result,
            };
            tokio::select! {
                biased;
                _ = attempt.cancelled.cancelled() => {},
                _ = completion_tx.send(LocalThreadCompletion::VerificationFinished {
                thread_id,
                server_name,
                request_id,
                attempt_id,
                result,
                }) => {},
            }
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finish_user_verification(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        server_name: String,
        request_id: RequestId,
        attempt_id: Uuid,
        result: Result<UserVerificationProof, UserVerificationFailure>,
    ) -> color_eyre::Result<()> {
        if self.abandoned_side_threads.contains(&thread_id)
            || !self
                .pending_app_server_requests
                .user_verification
                .is_pending(
                    &thread_id.to_string(),
                    &server_name,
                    &request_id,
                    attempt_id,
                )
        {
            return Ok(());
        }
        let response = match result {
            Ok(proof) => UserVerificationResponse::Accept { proof },
            Err(failure) => {
                self.enqueue_thread_notification(
                    thread_id,
                    ServerNotification::Warning(WarningNotification {
                        thread_id: Some(thread_id.to_string()),
                        message: failure.message.to_string(),
                    }),
                )
                .await?;
                // Another trusted device may be supported later. For now every incomplete
                // verification cancels the original elicitation without a fallback provider.
                UserVerificationResponse::Failed {
                    error: failure.details,
                }
            }
        };
        self.chat_widget
            .dismiss_app_server_request(&ResolvedAppServerRequest::McpElicitation {
                server_name: server_name.clone(),
                request_id: request_id.clone(),
            });
        self.submit_thread_op(
            app_server,
            thread_id,
            AppCommand::resolve_user_verification(server_name, request_id, response),
        )
        .await
    }
}

#[cfg(test)]
#[path = "user_verification_tests.rs"]
mod tests;
