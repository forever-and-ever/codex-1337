//! Signals raised by input queued while one sampling step is in flight.

use std::sync::Arc;

use codex_features::Feature;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use super::TurnInput;
use crate::config::ManagedFeatures;
use crate::session::session::Session;

/// Input queued during one sampling step that cuts the step's waiting short.
///
/// Created with the step and never reset. Once raised it stays raised for the rest of the
/// step, so a later Code Mode call in the same response returns at once instead of blocking
/// while the input that raised it waits for the next step.
#[derive(Clone)]
pub(crate) struct Interjection {
    /// Raised when user input is queued.
    steer: CancellationToken,
    /// Raised by a steer or released mail: a child of `steer`.
    news: CancellationToken,
}

impl Default for Interjection {
    fn default() -> Self {
        let steer = CancellationToken::new();
        let news = steer.child_token();
        Self { steer, news }
    }
}

impl Interjection {
    /// Ends blocked Code Mode `exec` and `wait` calls.
    pub(crate) fn news(&self) -> &CancellationToken {
        &self.news
    }

    /// Cuts off the step's streaming response: a steer under `Feature::InstantInterrupt`,
    /// otherwise never.
    pub(crate) fn cutoff(&self, features: &ManagedFeatures) -> CancellationToken {
        if features.enabled(Feature::InstantInterrupt) {
            self.steer.clone()
        } else {
            CancellationToken::new()
        }
    }

    /// Raises this interjection as input is queued for turn `sub_id`, until the handle drops.
    pub(crate) async fn watch(
        &self,
        sess: &Arc<Session>,
        sub_id: &str,
    ) -> Option<AbortOnDropHandle<()>> {
        let turn_state = sess
            .input_queue
            .turn_state_for_sub_id(&sess.active_turn, sub_id)
            .await?;
        // Subscribe before inspecting the queue so an arrival cannot be missed.
        let mut activity = sess.input_queue.activity_tx.subscribe();
        let sess = Arc::clone(sess);
        let steer = self.steer.clone();
        let news = self.news.clone();
        Some(AbortOnDropHandle::new(tokio::spawn(async move {
            loop {
                let (steered, mailed) = {
                    let turn_state = turn_state.lock().await;
                    let pending = &turn_state.pending_input;
                    (
                        pending.has_user_input(),
                        pending
                            .items
                            .iter()
                            .any(|input| matches!(input, TurnInput::InterAgentCommunication(_))),
                    )
                };
                if steered {
                    steer.cancel();
                    return;
                }
                if mailed || sess.input_queue.has_pending_mailbox_items().await {
                    news.cancel();
                }
                if activity.changed().await.is_err() {
                    return;
                }
            }
        })))
    }
}
