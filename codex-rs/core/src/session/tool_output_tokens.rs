//! Measure only accepted model-history results, never raw or nested code-mode tool returns.

use super::Session;
use super::TurnContext;
use codex_protocol::items::ToolOutputTokensItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_rollout::RolloutItem;

impl Session {
    pub(super) fn record_tool_output_tokens(
        &self,
        turn: &TurnContext,
        outputs: Vec<(String, FunctionCallOutputBody)>,
    ) {
        if outputs.is_empty() {
            return;
        }
        let thread_id = self.thread_id;
        let turn_id = turn.sub_id.clone();
        let live_thread = self.live_thread().cloned();
        let tx_event = self.get_tx_event();
        // These are observations, not execution barriers. In particular cold vocabulary
        // initialization must not hold up sampling, tool dispatch, or interruption.
        tokio::spawn(async move {
            let counts = tokio::task::spawn_blocking(move || {
                outputs
                    .into_iter()
                    .enumerate()
                    .map(|(index, (call_id, body))| {
                        ToolOutputTokensItem {
                            // A code-mode call can return multiple outputs through notify(). Each
                            // measurement needs its own persisted identity for idempotent replay.
                            id: format!("tool-output-tokens:{call_id}:{index}"),
                            call_id,
                            tokens: codex_utils_output_truncation::count_o200k_text_tokens(&body),
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .await;
            let Ok(counts) = counts else {
                tracing::warn!("tool output token measurement failed");
                return;
            };
            for item in counts {
                let msg = EventMsg::ItemCompleted(ItemCompletedEvent {
                    thread_id,
                    turn_id: turn_id.clone(),
                    item: TurnItem::ToolOutputTokens(item),
                    started_at_ms: None,
                    completed_at_ms: super::now_unix_timestamp_ms(),
                });
                if let Some(live_thread) = &live_thread
                    && let Err(error) = live_thread
                        .append_items(&[RolloutItem::EventMsg(msg.clone())])
                        .await
                {
                    tracing::debug!(%error, "could not persist tool output measurement");
                }
                // No execution lifecycle, legacy fanout, or parent-notification side effects.
                // Counts may arrive after turn completion; their turn and call identities survive.
                let _ = tx_event
                    .send(Event {
                        id: turn_id.clone(),
                        msg,
                    })
                    .await;
            }
        });
    }
}
