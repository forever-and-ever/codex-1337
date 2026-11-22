use super::*;
use crate::tools::registry::ToolArgumentDiffConsumer;
use codex_protocol::StreamItemsDiscardedEvent;
use std::collections::BTreeMap;

enum DeltaKind {
    Text,
    Reasoning,
    Arguments,
}

enum StreamItem {
    Deferred(TurnItem),
    Streaming(TurnItem),
    Arguments {
        call_id: String,
        consumer: Box<dyn ToolArgumentDiffConsumer>,
    },
    Opaque,
}

pub(super) struct StreamItems {
    items: BTreeMap<ResponseItemId, StreamItem>,
    pub(super) parsers: AssistantMessageStreamParsers,
    pub(super) plan: Option<PlanModeStreamState>,
}

pub(super) enum ItemCompletion {
    Streaming(TurnItem),
    ArgumentEvent(EventMsg),
    Silent,
}

impl ItemCompletion {
    pub(super) async fn emit(self, sess: &Session, turn: &TurnContext) -> Option<TurnItem> {
        match self {
            Self::Streaming(item) => Some(item),
            Self::ArgumentEvent(event) => {
                sess.send_event(turn, event).await;
                None
            }
            Self::Silent => None,
        }
    }
}

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

impl StreamItems {
    pub(super) fn new(plan_mode: bool, unfinished_plan_id: Option<String>) -> Self {
        Self {
            items: BTreeMap::new(),
            parsers: AssistantMessageStreamParsers::new(plan_mode),
            plan: plan_mode.then(|| PlanModeStreamState::new(unfinished_plan_id)),
        }
    }

    pub(super) fn started(&mut self, item: &ResponseItem) {
        if let Some(id) = item.id() {
            assert!(
                self.items.insert(id.clone(), StreamItem::Opaque).is_none(),
                "duplicate open response item"
            );
        }
    }

    pub(super) fn present(&mut self, id: ResponseItemId, item: TurnItem, streaming: bool) {
        self.items.insert(
            id,
            if streaming {
                StreamItem::Streaming(item)
            } else {
                StreamItem::Deferred(item)
            },
        );
    }

    pub(super) fn arguments(
        &mut self,
        id: ResponseItemId,
        call_id: String,
        consumer: Box<dyn ToolArgumentDiffConsumer>,
    ) {
        self.items
            .insert(id, StreamItem::Arguments { call_id, consumer });
    }

    pub(super) fn streaming(&self, id: &ResponseItemId) -> Option<&TurnItem> {
        match self.items.get(id) {
            Some(StreamItem::Streaming(item)) => Some(item),
            _ => None,
        }
    }

    pub(super) fn validate(&self, event: &ResponseEvent) -> CodexResult<()> {
        let (id, expected) = match event {
            ResponseEvent::OutputTextDelta { item_id, .. } => (item_id, DeltaKind::Text),
            ResponseEvent::ReasoningSummaryDelta { item_id, .. }
            | ResponseEvent::ReasoningSummaryDone { item_id, .. }
            | ResponseEvent::ReasoningContentDelta { item_id, .. }
            | ResponseEvent::ReasoningSummaryPartAdded { item_id, .. } => {
                (item_id, DeltaKind::Reasoning)
            }
            ResponseEvent::ToolCallInputDelta { item_id, .. } => (item_id, DeltaKind::Arguments),
            _ => return Ok(()),
        };
        let compatible = match (self.items.get(id), expected) {
            (Some(StreamItem::Streaming(item) | StreamItem::Deferred(item)), DeltaKind::Text) => {
                matches!(item, TurnItem::AgentMessage(_))
            }
            (
                Some(StreamItem::Streaming(item) | StreamItem::Deferred(item)),
                DeltaKind::Reasoning,
            ) => matches!(item, TurnItem::Reasoning(_)),
            (Some(StreamItem::Arguments { .. } | StreamItem::Opaque), DeltaKind::Arguments) => true,
            _ => false,
        };
        if compatible {
            Ok(())
        } else {
            Err(CodexErr::Stream(format!(
                "response delta has no compatible open item: {id}"
            )))
        }
    }

    pub(super) fn consume_arguments(
        &mut self,
        id: &ResponseItemId,
        call_id: Option<String>,
        delta: &str,
        turn: &TurnContext,
    ) -> Option<EventMsg> {
        let StreamItem::Arguments {
            call_id: expected,
            consumer,
        } = self.items.get_mut(id)?
        else {
            return None;
        };
        if call_id.as_ref().is_some_and(|id| id != expected) {
            return None;
        }
        consumer.consume_diff(turn, call_id.unwrap_or_else(|| expected.clone()), delta)
    }

    pub(super) fn recover_completion_id(&self, item: &mut ResponseItem) -> CodexResult<()> {
        if item.id().is_some_and(|id| !id.is_empty()) {
            return Ok(());
        }
        let mut open = self.items.keys();
        if let Some(id) = open.next() {
            if open.next().is_some() {
                return Err(CodexErr::Stream(
                    "response completion has no ID and several items are open".into(),
                ));
            }
            item.set_id(Some(id.clone()));
        }
        Ok(())
    }

    pub(super) fn completed(&mut self, item: &ResponseItem) -> ItemCompletion {
        let removed = item.id().and_then(|id| self.items.remove(id));
        match removed {
            Some(StreamItem::Streaming(item)) => ItemCompletion::Streaming(item),
            Some(StreamItem::Arguments { mut consumer, .. }) => consumer
                .finish()
                .ok()
                .flatten()
                .map_or(ItemCompletion::Silent, ItemCompletion::ArgumentEvent),
            _ => ItemCompletion::Silent,
        }
    }

    pub(super) fn fail(self, error: CodexErr) -> SamplingFailure {
        let mut unfinished = self
            .items
            .into_iter()
            .filter_map(|(id, entry)| {
                matches!(
                    entry,
                    StreamItem::Streaming(TurnItem::AgentMessage(_) | TurnItem::Reasoning(_))
                )
                .then(|| id.to_string())
            })
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(plan) = self.plan
            && plan.plan_item_state.started
            && !plan.plan_item_state.completed
        {
            unfinished.insert(plan.plan_item_state.item_id);
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
