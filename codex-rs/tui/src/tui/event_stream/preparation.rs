//! Lagged draw notifications coalesce; finite input bursts bound repaint starvation.

use super::Context;
use super::EventSource;
use super::MAX_INPUT_EVENTS_BETWEEN_DRAWS;
use super::Pin;
use super::Poll;
use super::SizeMonitor;
use super::Stream;
use super::TuiEvent;
use super::TuiEventStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

impl<S: EventSource + Default + Unpin> TuiEventStream<S> {
    pub(super) fn poll_prepared_draw(&mut self, cx: &mut Context<'_>) -> Poll<Option<TuiEvent>> {
        loop {
            match Pin::new(&mut self.draw_stream).poll_next(cx) {
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(_)))) => {}
                Poll::Ready(Some(Ok(()))) => {
                    let event = self
                        .broker
                        .size_monitor
                        .as_ref()
                        .and_then(SizeMonitor::take_resize)
                        .map_or(TuiEvent::Draw, TuiEvent::Resize);
                    return Poll::Ready(Some(event));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    pub(super) fn record_prepared_input(&mut self, event: &TuiEvent) {
        // Nested screens also receive finite input batches, never a full paint per key.
        self.input_events_since_draw = if matches!(event, TuiEvent::Draw) {
            0
        } else {
            self.input_events_since_draw.saturating_add(1)
        };
        self.poll_draw_first = self.input_events_since_draw >= MAX_INPUT_EVENTS_BETWEEN_DRAWS;
    }
}
