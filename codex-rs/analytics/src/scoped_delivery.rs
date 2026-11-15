//! One attribution reducer, with immutable per-thread delivery credentials.

use super::AnalyticsEventsDestination;
use super::RuntimeAuthMode;
use super::RuntimeAuthSlot;
use crate::events::TrackEventRequest;
use crate::product_attribution::MAX_THREAD_PRODUCTS;
use crate::product_attribution::ThreadProducts;
use crate::product_attribution::event_thread_id;
use codex_login::AuthManager;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;

pub(crate) struct DeliveryContext {
    pub(super) auth_manager: Arc<AuthManager>,
    pub(super) destination: AnalyticsEventsDestination,
    pub(super) runtime_auth: Option<(RuntimeAuthSlot, RuntimeAuthMode)>,
}

pub(super) struct DeliveryContexts {
    default: Arc<DeliveryContext>,
    threads: HashMap<String, Arc<DeliveryContext>>,
    order: VecDeque<String>,
}

impl DeliveryContexts {
    pub(super) fn new(default: Arc<DeliveryContext>) -> Self {
        Self {
            default,
            threads: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub(super) fn register(&mut self, thread: String, context: Option<Arc<DeliveryContext>>) {
        let context = context.unwrap_or_else(|| Arc::clone(&self.default));
        self.order.retain(|id| id != &thread);
        if !self.threads.contains_key(&thread)
            && self.threads.len() >= MAX_THREAD_PRODUCTS
            && let Some(oldest) = self.order.pop_front()
        {
            self.threads.remove(&oldest);
        }
        self.order.push_back(thread.clone());
        self.threads.insert(thread, context);
    }

    pub(super) async fn send(
        &self,
        events: Vec<TrackEventRequest>,
        products: &ThreadProducts,
        scoped: bool,
    ) {
        let mut batches: Vec<(Arc<DeliveryContext>, Vec<TrackEventRequest>)> = Vec::new();
        for event in events {
            let context = match event_thread_id(&event) {
                Some(thread) => match self.threads.get(thread) {
                    Some(context) => Arc::clone(context),
                    // A lost or evicted captured binding cannot spend the default account.
                    None if scoped => continue,
                    None => Arc::clone(&self.default),
                },
                None => Arc::clone(&self.default),
            };
            if let Some((_, batch)) = batches
                .iter_mut()
                .find(|(bound, _)| Arc::ptr_eq(bound, &context))
            {
                batch.push(event);
            } else {
                batches.push((context, vec![event]));
            }
        }
        for (context, events) in batches {
            super::send_track_events(
                &context.auth_manager,
                &context.destination,
                events,
                products,
                context.runtime_auth.as_ref(),
            )
            .await;
        }
    }
}
