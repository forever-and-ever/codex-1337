//! Retain startup hint facts until their thread attribution is published.

use super::AnalyticsDropSite;
use super::AnalyticsReducer;
use crate::events::TrackEventRequest;
use crate::thread_hint::ThreadHintStatusEvent;
use crate::thread_hint::ThreadHintStatusEventParams;
use crate::thread_hint::ThreadHintStatusEventRequest;

impl AnalyticsReducer {
    pub(super) fn ingest_thread_hint(
        &mut self,
        input: ThreadHintStatusEvent,
        out: &mut Vec<TrackEventRequest>,
    ) {
        if self.thread_connection_id(&input.thread_id).is_none()
            || self
                .threads
                .get(&input.thread_id)
                .is_none_or(|thread| thread.metadata.is_none())
        {
            if self.pending_thread_hints.len() >= super::MAX_TOOL_RESPONSE_ENTRIES {
                self.pending_thread_hints.pop_front();
                tracing::warn!("expiring oldest unattributed thread hint: state is full");
            }
            self.pending_thread_hints.push_back(input);
            return;
        }
        if let Some((connection, thread, metadata)) =
            self.thread_context_or_warn(AnalyticsDropSite {
                event_name: "codex_thread_hint_status",
                thread_id: &input.thread_id,
                turn_id: None,
                review_id: None,
                item_id: None,
            })
        {
            out.push(TrackEventRequest::ThreadHintStatus(Box::new(
                ThreadHintStatusEventRequest {
                    event_type: "codex_thread_hint_status",
                    event_params: ThreadHintStatusEventParams {
                        thread_id: input.thread_id,
                        session_id: metadata.session_id.clone(),
                        app_server_client: thread.app_server_client(connection),
                        runtime: connection.runtime.clone(),
                        thread_source: metadata.thread_source.clone(),
                        subagent_source: metadata.subagent_source.clone(),
                        parent_thread_id: metadata.parent_thread_id.clone(),
                        status: input.status,
                        occurred_at_ms: input.occurred_at_ms,
                    },
                },
            )));
        }
    }

    pub(super) fn flush_thread_hints(&mut self, out: &mut Vec<TrackEventRequest>) {
        // Parent initialization can complete attribution for already-started children.
        for input in std::mem::take(&mut self.pending_thread_hints) {
            self.ingest_thread_hint(input, out);
        }
    }
}
