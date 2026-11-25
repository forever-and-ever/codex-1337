use codex_history::RolloutItem;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsSnapshot;
use codex_rollout::InitialHistory;

/// Forks inherit a source's last acknowledged selection, not its paid job.
pub(super) fn inherited(
    history: &InitialHistory,
    thread: ThreadId,
) -> Option<&ThreadSettingsSnapshot> {
    let (items, owner) = match history {
        InitialHistory::Resumed(history) => (history.history.as_slice(), thread),
        InitialHistory::Forked(items) => {
            let owner = items.iter().rev().find_map(|item| match item {
                RolloutItem::SessionMeta(meta) => Some(meta.meta.id),
                _ => None,
            })?;
            (items.as_slice(), owner)
        }
        InitialHistory::New | InitialHistory::Cleared => return None,
    };
    items.iter().rev().find_map(|item| match item {
        RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event))
            if event.thread_id == Some(owner) =>
        {
            Some(&event.thread_settings)
        }
        _ => None,
    })
}
