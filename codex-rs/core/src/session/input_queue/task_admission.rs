//! Inspect wake attribution without transferring queue custody before task admission.

use super::InputQueue;
use super::PendingMailboxCommunication;
use super::TurnStartOptions;
use codex_protocol::protocol::InterAgentCommunication;

pub(super) fn start_options<'a>(
    mut pending: impl DoubleEndedIterator<Item = &'a PendingMailboxCommunication> + Clone,
) -> TurnStartOptions {
    // A later follow-up supersedes the earlier choice, including an omitted choice.
    let mut options = pending
        .clone()
        .rev()
        .find(|mail| mail.communication.trigger_turn)
        .map(|mail| mail.start_options.clone())
        .unwrap_or_default();
    options.parent_turn_id = pending
        .clone()
        .filter(|mail| mail.communication.trigger_turn)
        .map(|mail| mail.start_options.parent_turn_id.as_deref())
        .reduce(|expected, candidate| expected.filter(|id| candidate == Some(*id)))
        .and_then(|id| id.filter(|id| !id.trim().is_empty()).map(str::to_string));
    options.root_turn_id = pending
        .find(|mail| mail.communication.trigger_turn)
        .and_then(|mail| {
            mail.start_options
                .parent_turn_id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
                .and(mail.start_options.root_turn_id.as_deref())
                .filter(|id| !id.trim().is_empty())
        })
        .map(str::to_string);
    options
}

impl InputQueue {
    pub(crate) async fn pending_mailbox_snapshot(
        &self,
    ) -> (Vec<InterAgentCommunication>, TurnStartOptions) {
        let mut pending = self.mailbox_pending_mails.lock().await;
        self.read_mailbox(&mut pending.ready);
        (
            pending
                .ready
                .iter()
                .map(|mail| mail.communication.clone())
                .collect(),
            start_options(pending.ready.iter()),
        )
    }
}
