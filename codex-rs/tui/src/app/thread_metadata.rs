//! Background writes for metadata discovered by the frontend.

use super::App;
use super::turn_submissions::ui_requests::UiRequestKind;
use super::turn_submissions::ui_requests::UiRequestScope;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::typed_requests::SessionRpc;
use codex_app_server_protocol as api;
use codex_protocol::ThreadId;

impl App {
    pub(super) fn sync_thread_git_branch(
        &mut self,
        server: &AppServerSession,
        thread_id: ThreadId,
        branch: String,
    ) {
        self.request_server(
            server,
            UiRequestScope::Thread(thread_id),
            UiRequestKind::Mutation,
            "Syncing thread git branch",
            move |handle| async move {
                handle
                    .thread_metadata_update_branch(
                        api::RequestId::String(uuid::Uuid::new_v4().to_string()),
                        api::ThreadMetadataUpdateParams {
                            thread_id: thread_id.to_string(),
                            project_id: None,
                            daybreak_enabled: None,
                            comms: None,
                            git_info: Some(api::ThreadMetadataGitInfoUpdateParams {
                                sha: None,
                                branch: Some(Some(branch)),
                                origin_url: None,
                            }),
                        },
                    )
                    .await
            },
            |_, _, result| {
                if let Err(error) = result {
                    tracing::warn!(%error, "failed to sync thread git branch from directive");
                }
                Ok(())
            },
        );
    }

    /// The connection barrier prevents later navigation from reacquiring an
    /// old subscription until this batch settles. Captured stores stay in
    /// custody across the local attachment; late replies only report failures.
    pub(super) fn unsubscribe_overview_threads(
        &mut self,
        server: &AppServerSession,
        mut thread_ids: Vec<ThreadId>,
    ) {
        let mut seen = std::collections::HashSet::new();
        thread_ids.retain(|id| seen.insert(*id));
        if thread_ids.is_empty() {
            return;
        }
        let targets = thread_ids
            .into_iter()
            .map(|id| {
                (
                    id,
                    std::sync::Arc::clone(&self.ensure_thread_channel(id).store),
                )
            })
            .collect::<Vec<_>>();
        self.request_server(
                server, UiRequestScope::Connection, UiRequestKind::Mutation, "Detaching previous overview threads",
                move |handle| async move {
                    let mut results = Vec::new();
                    for (thread_id, _) in &targets {
                        let result = handle.request_typed::<api::ThreadUnsubscribeResponse>(api::ClientRequest::ThreadUnsubscribe {
                            request_id: api::RequestId::String(uuid::Uuid::new_v4().to_string()),
                            params: api::ThreadUnsubscribeParams { thread_id: thread_id.to_string() },
                        }).await;
                        if let Err(error @ (codex_app_server_client::TypedRequestError::Transport { .. } | codex_app_server_client::TypedRequestError::Deserialize { .. })) = result {
                            return Err(error);
                        }
                        results.push((*thread_id, result));
                    }
                    Ok((targets, results))
                },
                |_, _, result| {
                    match result {
                        Ok((_custody, results)) => for (thread_id, result) in results {
                            if let Err(error) = result {
                                tracing::warn!(%thread_id, %error, "previous overview unsubscribe not confirmed; no retry was sent");
                            }
                        },
                        Err(error) => tracing::warn!(%error, "previous overview detach batch did not complete; no retry was sent"),
                    }
                    Ok(())
                },
            );
    }
}
