//! An owned release channel closes a wait on return, error, or dropped invocation.

use super::super::CodeModeService;
use crate::session::turn_context::TurnContext;
use codex_protocol::code_mode::CodeModeEvent;
use codex_protocol::code_mode::CodeModeOwner;
use std::sync::Arc;
use std::sync::PoisonError;
use tokio::sync::oneshot;

pub(crate) struct ProcessWait {
    release: oneshot::Sender<()>,
    closed: oneshot::Receiver<()>,
}

impl ProcessWait {
    pub(crate) async fn finish(self) {
        let Self { release, closed } = self;
        drop(release);
        closed
            .await
            .expect("process wait tracker must close its lease");
    }
}

impl CodeModeService {
    pub(crate) async fn begin_process_wait(
        &self,
        call_id: &str,
        target: CodeModeOwner,
        turn: Arc<TurnContext>,
    ) -> Option<ProcessWait> {
        let scope = {
            let state = self
                .transcript
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let alias = state.aliases.get(call_id)?;
            if !alias.anchor.alive() {
                return None;
            }
            alias.scope.upgrade()?
        };
        let call_id = call_id.to_owned();
        let (release, released) = oneshot::channel();
        let (opened, admitted) = oneshot::channel();
        let (close, closed) = oneshot::channel();
        let wait = ProcessWait { release, closed };
        self.transcript.tasks.spawn(async move {
            scope
                .record(
                    &turn,
                    CodeModeEvent::WaitStarted {
                        call_id: call_id.clone(),
                        target: Some(target.clone()),
                    },
                )
                .await;
            let _ = opened.send(());
            tokio::select! {
                _ = released => {},
                _ = scope.shutdown.cancelled() => {},
            }
            scope
                .record(
                    &turn,
                    CodeModeEvent::WaitCompleted {
                        call_id,
                        target: Some(target),
                    },
                )
                .await;
            let _ = close.send(());
        });
        admitted
            .await
            .expect("process wait tracker must admit its lease");
        Some(wait)
    }
}
