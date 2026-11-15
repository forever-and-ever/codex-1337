//! Captured deletion authority: finish all sidebar preflight before mutating any thread.

use super::AppServerSession;
use super::ThreadParamsMode;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadDeleteParams;
use codex_app_server_protocol::ThreadDeleteResponse;
use codex_protocol::ThreadId;
use color_eyre::eyre::Report;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct ThreadDeletion {
    handle: AppServerRequestHandle,
    mode: ThreadParamsMode,
    parent: ThreadId,
    inspect_saved_side_chats: bool,
}

#[derive(Debug)]
pub(crate) enum ThreadDeletionError {
    /// No mutation has been sent. Preserve local validation and read-RPC failures.
    Preflight(Report),
    /// A deletion request was sent; its acknowledgement determines certainty.
    Request(TypedRequestError),
}

impl AppServerSession {
    pub(crate) fn prepare_thread_delete(&self, parent: ThreadId) -> ThreadDeletion {
        ThreadDeletion {
            handle: self.request_handle(),
            mode: self.thread_params_mode(),
            parent,
            inspect_saved_side_chats: self.can_list_saved_side_chats(parent),
        }
    }
}

impl ThreadDeletion {
    pub(crate) async fn run(
        self,
        request_id: RequestId,
    ) -> Result<ThreadDeleteResponse, ThreadDeletionError> {
        let children = if self.inspect_saved_side_chats {
            let mut view = AppServerSession::from_parts(
                crate::backend_id::BackendId::new(),
                None,
                self.handle.clone(),
                self.mode,
            );
            view.saved_side_chat_children(self.parent)
                .await
                .map_err(ThreadDeletionError::Preflight)?
        } else {
            Vec::new()
        };
        for child in children {
            self.handle
                .request_typed::<ThreadDeleteResponse>(ClientRequest::ThreadDelete {
                    request_id: RequestId::String(format!("tui-delete-child-{}", Uuid::new_v4())),
                    params: ThreadDeleteParams {
                        thread_id: child.to_string(),
                    },
                })
                .await
                .map_err(ThreadDeletionError::Request)?;
        }
        self.handle
            .request_typed(ClientRequest::ThreadDelete {
                request_id,
                params: ThreadDeleteParams {
                    thread_id: self.parent.to_string(),
                },
            })
            .await
            .map_err(ThreadDeletionError::Request)
    }
}
