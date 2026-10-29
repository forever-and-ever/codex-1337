//! Persisted turn subtotals outlive foreground execution only while that turn still owns work.

use std::sync::Mutex;

use super::SessionState;
use codex_history::RolloutItem;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageRecord;

/// Shared by a turn's foreground and background contexts, never by different turns.
#[derive(Debug, Default)]
pub(crate) struct TurnSubtotal(Mutex<Option<TokenUsage>>);

// TELOS(interleaved-turn-accounting): another turn's late response cannot reset this subtotal.
pub(super) fn record(
    state: &mut SessionState,
    subtotal: &TurnSubtotal,
    thread_id: ThreadId,
    turn_id: &str,
    session_id: SessionId,
    root_turn_id: String,
    response_id: String,
    usage: &TokenUsage,
) -> TokenUsageRecord {
    #[expect(
        clippy::expect_used,
        reason = "poisoned accounting is an invariant failure, never recoverable usage"
    )]
    let mut subtotal = subtotal.0.lock().expect("turn subtotal lock poisoned");
    let turn_token_usage = subtotal.get_or_insert_with(|| {
        state
            .turn_usage_seed
            .take()
            .filter(|record| record.turn_id == turn_id)
            .or_else(|| {
                state
                    .latest_token_usage_record
                    .as_ref()
                    .filter(|record| record.turn_id == turn_id)
                    .cloned()
            })
            .map_or_else(TokenUsage::default, |record| record.turn_token_usage)
    });
    turn_token_usage.add_assign(usage);
    let mut thread_token_usage = state
        .latest_token_usage_record
        .as_ref()
        .map_or_else(TokenUsage::default, |record| {
            record.thread_token_usage.clone()
        });
    thread_token_usage.add_assign(usage);
    let record = TokenUsageRecord {
        thread_id,
        turn_id: turn_id.to_owned(),
        session_id,
        root_turn_id,
        response_id,
        usage: usage.clone(),
        turn_token_usage: turn_token_usage.clone(),
        thread_token_usage,
    };
    state.latest_token_usage_record = Some(record.clone());
    record
}

/// A cold session has no live background work; only its recoverable last turn needs a seed.
pub(crate) fn restore(state: &mut SessionState, items: &[RolloutItem]) {
    state.turn_usage_seed = state.last_started_turn_id.as_deref().and_then(|turn_id| {
        for item in items.iter().rev() {
            match item {
                RolloutItem::TokenUsageRecord(record) if record.turn_id == turn_id => {
                    return Some(record.clone());
                }
                RolloutItem::Compacted(compacted) => {
                    return compacted
                        .latest_token_usage_record
                        .as_ref()
                        .filter(|record| record.turn_id == turn_id)
                        .cloned();
                }
                _ => {}
            }
        }
        None
    });
}
