//! Pending accepted mail, not a frontend or one-generation shutdown snapshot, drives recovery.

use super::ServerResume;
use super::ThreadRequestProcessor;
use super::ThreadResumeTarget;
use crate::turn_admission::TurnAdmission;
use codex_app_server_protocol::ThreadResumeParams;
use codex_protocol::ThreadId;
use codex_protocol::mcp::ClientMcpExtensions;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::task::AbortOnDropHandle;

struct RecoveryBackoff {
    delay: Duration,
    not_before: Instant,
    diagnostic: String,
}

impl RecoveryBackoff {
    fn new(error: impl std::fmt::Display) -> Self {
        let delay = Duration::from_secs(1);
        Self {
            delay,
            not_before: Instant::now() + delay,
            diagnostic: error.to_string(),
        }
    }

    fn waiting(&self) -> bool {
        Instant::now() < self.not_before
    }

    fn defer(&mut self, error: impl std::fmt::Display) -> bool {
        let diagnostic = error.to_string();
        let changed = diagnostic != self.diagnostic;
        self.diagnostic = diagnostic;
        self.delay = (self.delay * 2).min(Duration::from_mins(1));
        self.not_before = Instant::now() + self.delay;
        changed
    }
}

fn defer_recipient(
    retries: &mut HashMap<(ThreadId, ThreadId), RecoveryBackoff>,
    root: ThreadId,
    recipient: ThreadId,
    error: impl std::fmt::Display,
) {
    match retries.entry((root, recipient)) {
        Entry::Vacant(entry) => {
            entry.insert(RecoveryBackoff::new(&error));
            tracing::error!(%root, %recipient, %error, "accepted-mail recovery is blocked; inbox retained; unload/reload or resume recipient after repairing storage; retrying with capped backoff");
        }
        Entry::Occupied(mut entry) => {
            if entry.get_mut().defer(&error) {
                tracing::error!(%root, %recipient, %error, "accepted-mail recovery blockage changed; inbox retained");
            } else {
                tracing::debug!(%root, %recipient, %error, "accepted-mail recovery remains blocked; inbox retained");
            }
        }
    }
}

impl ThreadRequestProcessor {
    pub(crate) fn start_accepted_mail_recovery(
        &self,
        admission: TurnAdmission,
    ) -> AbortOnDropHandle<()> {
        let processor = self.clone();
        let task: JoinHandle<()> = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut recipients = HashMap::new();
            let mut inbox_retry: Option<RecoveryBackoff> = None;
            loop {
                tick.tick().await;
                if inbox_retry.as_ref().is_some_and(RecoveryBackoff::waiting) {
                    continue;
                }
                match Box::pin(processor.recover_accepted_mail(&admission, &mut recipients)).await {
                    Ok(()) => inbox_retry = None,
                    Err(error) => {
                        if let Some(retry) = &mut inbox_retry {
                            if retry.defer(&error) {
                                tracing::error!(%error, "accepted agent-mail inbox recovery blockage changed; inbox retained");
                            } else {
                                tracing::debug!(%error, "accepted agent-mail inbox recovery remains blocked; inbox retained");
                            }
                        } else {
                            inbox_retry = Some(RecoveryBackoff::new(&error));
                            tracing::error!(%error, "accepted agent-mail inbox recovery is blocked; inbox retained; retrying with capped backoff");
                        }
                    }
                }
            }
        });
        AbortOnDropHandle::new(task)
    }

    async fn recover_accepted_mail(
        &self,
        admission: &TurnAdmission,
        retries: &mut HashMap<(ThreadId, ThreadId), RecoveryBackoff>,
    ) -> anyhow::Result<()> {
        let db = self
            .state_db
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("authoritative agent-mail inbox is unavailable"))?;
        let recipients = db.thread_queue().pending_agent_mail_recipients().await?;
        let pending: HashSet<_> = recipients.iter().copied().collect();
        retries.retain(|identity, _| pending.contains(identity));
        for (root, recipient) in recipients {
            if retries
                .get(&(root, recipient))
                .is_some_and(RecoveryBackoff::waiting)
            {
                continue;
            }
            let Ok(_permit) = admission.admit() else {
                return Ok(());
            };
            if self.thread_manager.get_thread(root).await.is_err()
                && let Err(error) = Box::pin(self.thread_resume(
                    ThreadResumeTarget::Server(ServerResume::DaemonRecovery(None)),
                    ThreadResumeParams {
                        thread_id: root.to_string(),
                        exclude_turns: true,
                        ..Default::default()
                    },
                    None,
                    None,
                    ClientMcpExtensions::default(),
                ))
                .await
            {
                defer_recipient(retries, root, recipient, &error.message);
                continue;
            }
            // Core restores descendants through the recorded family and current authority.
            // Neither an absent client nor a successful resume supplies approval.
            if let Err(error) = Box::pin(
                self.thread_manager
                    .recover_accepted_agent_mail(root, recipient),
            )
            .await
            {
                defer_recipient(retries, root, recipient, error);
            } else {
                retries.remove(&(root, recipient));
            }
        }
        Ok(())
    }
}
