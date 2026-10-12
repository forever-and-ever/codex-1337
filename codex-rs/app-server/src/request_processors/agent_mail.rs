//! Pending accepted mail, not a frontend or one-generation shutdown snapshot, drives recovery.

use super::ServerResume;
use super::ThreadRequestProcessor;
use super::ThreadResumeTarget;
use crate::turn_admission::TurnAdmission;
use codex_app_server_protocol::ThreadResumeParams;
use codex_protocol::mcp::ClientMcpExtensions;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::task::AbortOnDropHandle;

impl ThreadRequestProcessor {
    pub(crate) fn start_accepted_mail_recovery(
        &self,
        admission: TurnAdmission,
    ) -> AbortOnDropHandle<()> {
        let processor = self.clone();
        let task: JoinHandle<()> = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if let Err(error) = processor.recover_accepted_mail(&admission).await {
                    tracing::error!(%error, "accepted agent-mail recovery is blocked; authoritative inbox retained");
                }
            }
        });
        AbortOnDropHandle::new(task)
    }

    async fn recover_accepted_mail(&self, admission: &TurnAdmission) -> anyhow::Result<()> {
        let db = self
            .state_db
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("authoritative agent-mail inbox is unavailable"))?;
        let recipients = db.thread_queue().pending_agent_mail_recipients().await?;
        for (root, recipient) in recipients {
            let _permit = match admission.admit() {
                Ok(permit) => permit,
                Err(_) => return Ok(()),
            };
            if self.thread_manager.get_thread(root).await.is_err() {
                if let Err(error) = self
                    .thread_resume(
                        ThreadResumeTarget::Server(ServerResume::DaemonRecovery(None)),
                        ThreadResumeParams {
                            thread_id: root.to_string(),
                            exclude_turns: true,
                            ..Default::default()
                        },
                        None,
                        None,
                        ClientMcpExtensions::default(),
                    )
                    .await
                {
                    tracing::error!(%root, message = %error.message, "cannot restore accepted-mail family; inbox retained");
                    continue;
                }
            }
            // Core restores descendants through the recorded family and current authority.
            // Neither an absent client nor a successful resume supplies approval.
            if let Err(error) = self
                .thread_manager
                .recover_accepted_agent_mail(root, recipient)
                .await
            {
                tracing::error!(%root, %recipient, %error, "accepted mail awaits family recovery");
            }
        }
        Ok(())
    }
}
