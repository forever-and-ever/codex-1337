use super::OutgoingEnvelope;
use super::OutgoingMessageSender;
use futures::future::BoxFuture;
use futures::future::pending;
use tokio::sync::oneshot;

#[derive(Default)]
pub(crate) enum ClientDisconnect {
    #[default]
    Open,
    Flushing(BoxFuture<'static, ()>),
    Closed,
}

impl ClientDisconnect {
    pub(crate) fn begin(&mut self, outgoing: &OutgoingMessageSender) {
        if !matches!(self, Self::Open) {
            return;
        }
        let sender = outgoing.sender.clone();
        *self = Self::Flushing(Box::pin(async move {
            let (write_complete_tx, written) = oneshot::channel();
            if sender
                .send(OutgoingEnvelope::DisconnectAll { write_complete_tx })
                .await
                .is_ok()
            {
                // A dropped receipt means the router or transport already closed.
                let _ = written.await;
            }
        }));
    }

    pub(crate) async fn flushed(&mut self) {
        match self {
            Self::Flushing(receipt) => receipt.await,
            Self::Open | Self::Closed => pending().await,
        }
        *self = Self::Closed;
    }

    pub(crate) fn closed(&self) -> bool {
        matches!(self, Self::Closed)
    }
}
