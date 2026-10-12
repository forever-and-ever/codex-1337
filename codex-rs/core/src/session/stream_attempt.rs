use super::*;
use codex_protocol::StreamItemsDiscardedEvent;

pub(super) struct SamplingFailure {
    pub(super) error: CodexErr,
    pub(super) unfinished: Vec<String>,
}

impl From<CodexErr> for SamplingFailure {
    fn from(error: CodexErr) -> Self {
        Self {
            error,
            unfinished: Vec::new(),
        }
    }
}

impl SamplingFailure {
    pub(super) fn capture(
        error: CodexErr,
        active: Option<&TurnItem>,
        plan: Option<&PlanModeStreamState>,
    ) -> Self {
        let mut unfinished = Vec::new();
        if let Some(item @ (TurnItem::AgentMessage(_) | TurnItem::Reasoning(_))) = active {
            unfinished.push(item.id());
        }
        if let Some(plan) = plan
            && plan.plan_item_state.started
            && !plan.plan_item_state.completed
        {
            unfinished.push(plan.plan_item_state.item_id.clone());
        }
        Self { error, unfinished }
    }
}

pub(super) async fn discard(sess: &Session, turn: &TurnContext, item_ids: Vec<String>) {
    if item_ids.is_empty() {
        return;
    }
    sess.send_event(
        turn,
        EventMsg::StreamItemsDiscarded(StreamItemsDiscardedEvent {
            thread_id: sess.thread_id,
            turn_id: turn.sub_id.clone(),
            item_ids,
        }),
    )
    .await;
}
