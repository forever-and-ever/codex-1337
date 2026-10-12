use super::*;
use codex_protocol::StreamItemsDiscardedEvent;

use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct UnfinishedItems(BTreeSet<String>);

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

impl UnfinishedItems {
    pub(super) fn started(&mut self, item: &TurnItem) {
        if matches!(item, TurnItem::AgentMessage(_) | TurnItem::Reasoning(_)) {
            self.0.insert(item.id());
        }
    }

    pub(super) fn completed(&mut self, item: &ResponseItem) {
        if let Some(id) = item.id() {
            self.0.remove(id.as_str());
        }
    }

    pub(super) fn fail(
        self,
        error: CodexErr,
        plan: Option<&PlanModeStreamState>,
    ) -> SamplingFailure {
        let Self(mut unfinished) = self;
        if let Some(plan) = plan
            && plan.plan_item_state.started
            && !plan.plan_item_state.completed
        {
            unfinished.insert(plan.plan_item_state.item_id.clone());
        }
        SamplingFailure {
            error,
            unfinished: unfinished.into_iter().collect(),
        }
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
