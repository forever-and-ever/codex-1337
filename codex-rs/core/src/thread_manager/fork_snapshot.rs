//! Fork boundaries distinguish a copied unfinished turn from a real interruption.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkSnapshot {
    /// Fork a committed prefix ending strictly before the nth user message.
    ///
    /// When `n` is within range, this cuts before that 0-based user-message
    /// boundary. When `n` is out of range and the source thread is currently
    /// mid-turn, this instead cuts before the active turn's opening boundary
    /// so the fork drops the unfinished turn suffix. When `n` is out of range
    /// and the source thread is already at a turn boundary, this returns the
    /// full committed history unchanged.
    TruncateBeforeNthUserMessage(usize),

    /// Fork the current persisted history as if the source thread had been
    /// interrupted now, using the configured model-visible interrupt marker.
    Interrupted,

    /// Close only the copied turn for bookkeeping; the source keeps running.
    /// The caller supplies the side boundary as one appended user message.
    Side,
}

pub(super) fn fork_history_from_snapshot(
    snapshot: ForkSnapshot,
    history: InitialHistory,
    interrupted_marker: InterruptedTurnHistoryMarker,
) -> InitialHistory {
    let snapshot_state = snapshot_turn_state(&history);
    match snapshot {
        ForkSnapshot::TruncateBeforeNthUserMessage(nth_user_message) => {
            truncate_before_nth_user_message(history, nth_user_message, &snapshot_state)
        }
        ForkSnapshot::Interrupted | ForkSnapshot::Side => {
            let history = match history {
                InitialHistory::New => InitialHistory::New,
                InitialHistory::Cleared => InitialHistory::Cleared,
                InitialHistory::Forked(history) => InitialHistory::Forked(history),
                InitialHistory::Resumed(resumed) => {
                    InitialHistory::Forked(Arc::unwrap_or_clone(resumed.history))
                }
            };
            if snapshot_state.ends_mid_turn {
                append_interrupted_boundary(
                    history,
                    snapshot_state.active_turn_id,
                    snapshot_state.active_turn_root_id,
                    snapshot_state.active_turn_started_at,
                    match snapshot {
                        ForkSnapshot::Side => InterruptedTurnHistoryMarker::Disabled,
                        _ => interrupted_marker,
                    },
                )
            } else {
                history
            }
        }
    }
}

impl ThreadManager {
    pub async fn fork_prepared_thread_with_snapshot(
        &self,
        snapshot: ForkSnapshot,
        options: StartThreadOptions,
        prepared: PreparedFork,
    ) -> CodexResult<NewThread> {
        let history = InitialHistory::Resumed(ResumedHistory {
            history_revision: None,
            conversation_id: prepared.source_thread_id,
            history: Arc::clone(&prepared.model_context),
            rollout_path: None,
        });
        let fork_persistence = ForkPersistence::Referenced {
            history_base: prepared.history_base,
            inherited_item_count: prepared.model_context.len(),
        };
        let result = self
            .fork_thread_with_initial_history(
                options,
                ForkHistory {
                    snapshot,
                    initial_history: history,
                    persistence: fork_persistence,
                },
            )
            .await;
        drop(prepared);
        result
    }
}
