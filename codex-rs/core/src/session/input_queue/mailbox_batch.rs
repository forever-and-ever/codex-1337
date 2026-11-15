//! One fixed delivery deadline per recipient, owned by its submission loop.
//! Family execution permits, not ancestry or frontend state, decide whether to hold mail.

use super::InputQueue;
use super::PendingMailboxCommunication;
use crate::agent::control::FamilyExecution;
use crate::agent_communication::PENDING_MAILBOX_MESSAGES;
use crate::session::session::Session;
use codex_protocol::turn_input::TurnStartOptions;
use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio::time::sleep_until;

#[derive(Default)]
pub(super) struct MailboxQueue {
    pub(super) ready: VecDeque<PendingMailboxCommunication>,
    batch: MailboxBatch,
}

#[derive(Default)]
enum MailboxBatch {
    #[default]
    Empty,
    Holding {
        deadline: Instant,
        release_generation: u64,
        mail: VecDeque<PendingMailboxCommunication>,
    },
}

impl MailboxQueue {
    pub(super) fn release(&mut self) -> bool {
        match std::mem::take(&mut self.batch) {
            MailboxBatch::Empty => false,
            MailboxBatch::Holding { mut mail, .. } => {
                self.ready.append(&mut mail);
                true
            }
        }
    }
}

impl Session {
    pub(crate) async fn receive_agent_mail(
        self: &Arc<Self>,
        communication: crate::PendingAgentMessage,
        start_options: TurnStartOptions,
        submission_id: String,
    ) {
        match self.receive_accepted_agent_mail(&communication).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::error!(%error, "accepted agent-mail publication is blocked; durable inbox retained");
                return;
            }
        }
        let accepted_mail_id = communication.id.clone();
        let config = self.get_config().await;
        let active = *self
            .services
            .local_agent_runtime
            .active_subagents()
            .borrow();
        self.input_queue
            .enqueue_batched_mailbox_communication(
                communication,
                start_options,
                config.multi_agent_v2.wait_agent_batch_window_ms,
                active,
            )
            .await;
        self.agent_mail_projection_published(accepted_mail_id.as_ref())
            .await;
        crate::agent_communication::emit_agent_communication_receive(&submission_id);
        if self.input_queue.has_pending_mailbox_items().await {
            Box::pin(self.maybe_start_turn_for_pending_work_with_sub_id(submission_id)).await;
        }
    }

    pub(crate) async fn mailbox_turn_has_capacity(&self) -> bool {
        let state = self.state.lock().await;
        let config = &state.session_configuration.original_config_do_not_use;
        let version = self
            .multi_agent_version()
            .unwrap_or_else(|| config.multi_agent_version_from_features());
        // Execution-guard releases retry accepted mail that cannot yet fit.
        self.services
            .agent_control
            .check_turn_admission(version, &state.session_configuration.session_source)
            .is_ok()
    }
}

impl InputQueue {
    pub(crate) async fn enqueue_batched_mailbox_communication(
        &self,
        communication: impl Into<crate::PendingAgentMessage>,
        start_options: TurnStartOptions,
        window: Option<NonZeroU64>,
        execution: FamilyExecution,
    ) {
        let communication = communication.into();
        let window = window.filter(|_| {
            execution.active >= 2 && !communication.author.is_root() && !communication.trigger_turn
        });
        let Some(window) = window else {
            self.enqueue_mailbox_communication(communication, start_options)
                .await;
            return;
        };
        let mut batch = self.mailbox_pending_mails.lock().await;
        batch.batch.push(
            PendingMailboxCommunication {
                communication,
                start_options,
                _diagnostics_guard: PENDING_MAILBOX_MESSAGES.track(),
            },
            window,
            execution.release_generation,
        );
        self.mailbox_batch_changed.notify_one();
    }

    pub(crate) async fn has_held_mailbox_items(&self) -> bool {
        matches!(
            self.mailbox_pending_mails.lock().await.batch,
            MailboxBatch::Holding { .. }
        )
    }

    pub(crate) async fn release_mailbox_batch(&self) -> bool {
        // Moving held mail and publishing it is one cancellation-safe queue transition.
        let released = self.mailbox_pending_mails.lock().await.release();
        if released {
            self.notify_mailbox();
        }
        released
    }

    pub(crate) async fn wait_for_mailbox_release(
        &self,
        active: &mut watch::Receiver<FamilyExecution>,
    ) {
        loop {
            // Arm before reading the batch: a new first arrival cannot lose its wake.
            let changed = self.mailbox_batch_changed.notified();
            let batch = match &self.mailbox_pending_mails.lock().await.batch {
                MailboxBatch::Empty => None,
                MailboxBatch::Holding {
                    deadline,
                    release_generation,
                    ..
                } => Some((*deadline, *release_generation)),
            };
            let execution = *active.borrow();
            if batch.is_some_and(|(deadline, generation)| {
                execution.active < 2
                    || execution.release_generation != generation
                    || deadline <= Instant::now()
            }) {
                self.release_mailbox_batch().await;
                return;
            }
            tokio::select! {
                () = changed => {}
                result = active.changed() => {
                    let execution = *active.borrow();
                    if result.is_err() || execution.active < 2
                        || batch.is_some_and(|(_, generation)| execution.release_generation != generation)
                    {
                        self.release_mailbox_batch().await;
                    }
                    // Also retry ordinary-mail admission when execution capacity is released.
                    return;
                }
                () = async {
                    match batch {
                        Some((deadline, _)) => sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
        }
    }
}

impl MailboxBatch {
    fn push(
        &mut self,
        mail: PendingMailboxCommunication,
        window: NonZeroU64,
        release_generation: u64,
    ) {
        match self {
            Self::Empty => {
                *self = Self::Holding {
                    deadline: Instant::now() + Duration::from_millis(window.get()),
                    release_generation,
                    mail: VecDeque::from([mail]),
                }
            }
            Self::Holding { mail: pending, .. } => pending.push_back(mail),
        }
    }
}

impl InputQueue {
    pub(crate) fn can_prove_mailbox_empty(&self) -> Option<bool> {
        self.mailbox_pending_mails.try_lock().ok().map(|pending| {
            pending.ready.is_empty()
                && !matches!(pending.batch, MailboxBatch::Holding { .. })
                && self
                    .controller
                    .as_ref()
                    .is_none_or(|(_, _, updates)| !*updates.borrow())
        })
    }

    pub(crate) fn can_prove_mailbox_retainable(&self) -> Option<bool> {
        self.mailbox_pending_mails.try_lock().ok().map(|pending| {
            let batch_ready = match &pending.batch {
                MailboxBatch::Empty => true,
                MailboxBatch::Holding { mail, .. } => {
                    mail.iter().all(|mail| !mail.communication.trigger_turn)
                }
            };
            batch_ready
                && pending
                    .ready
                    .iter()
                    .all(|mail| !mail.communication.trigger_turn)
                && self
                    .controller
                    .as_ref()
                    .is_none_or(|(_, _, updates)| !*updates.borrow())
        })
    }
}
