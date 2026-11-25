use std::future::Future;

use codex_protocol::error::Result as CodexResult;
use codex_protocol::turn_input::StartIfIdleSubmission;
use codex_protocol::turn_input::TurnInputMode;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::turn_input::TurnInputSubmission;

use crate::session::queued_input::QueuedInputAdmission;

use super::CodexThread;
use super::resolve_client_turn_input;

impl CodexThread {
    /// Look up a queue row's durable disposition.
    pub async fn queued_input_turn_id(&self, id: &str) -> CodexResult<Option<String>> {
        self.session.queued_input_turn_id(id).await
    }

    /// Started only reports routing. Core invokes cleanup after a canonical receipt
    /// or an explicit prompt-hook veto; all earlier failures leave the queue row alone.
    pub async fn start_queued_input_if_idle<F>(
        &self,
        request: TurnInputRequest,
        id: String,
        cleanup: impl FnOnce() -> F + Send + 'static,
    ) -> CodexResult<StartIfIdleSubmission>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if let Some(turn_id) = self.queued_input_turn_id(&id).await? {
            return Ok(StartIfIdleSubmission::Started {
                root_turn_id: turn_id.clone(),
                turn_id,
            });
        }
        let request = resolve_client_turn_input(request).await?;
        self.ensure_execution_capacity_for_turn_start(self.session.services.agent_control.as_ref())
            .await?;
        let admission = QueuedInputAdmission::new(id, cleanup);
        match self
            .io
            .submit_turn_input_with_queue(request, TurnInputMode::StartIfIdle, Some(admission))
            .await?
        {
            TurnInputSubmission::Started {
                turn_id,
                root_turn_id,
            } => Ok(StartIfIdleSubmission::Started {
                turn_id,
                root_turn_id,
            }),
            TurnInputSubmission::NotSubmitted { reason } => {
                Ok(StartIfIdleSubmission::NotSubmitted { reason })
            }
            TurnInputSubmission::Steered { .. } | TurnInputSubmission::Accepted => {
                unreachable!("queued input starts only when idle")
            }
        }
    }
}
