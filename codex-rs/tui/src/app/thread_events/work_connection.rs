//! A cached Work projection is evidence from one connection, never its successor.

use super::ThreadEventAttachment;
use super::ThreadEventChannel;
use super::ThreadEventStore;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::app) struct ConnectionEpoch(u64);

impl ConnectionEpoch {
    #[expect(
        clippy::expect_used,
        reason = "The connection epoch cannot exhaust u64 within one frontend lifetime."
    )]
    pub(in crate::app) fn advance(&mut self) {
        self.0 = self.0.checked_add(1).expect("connection epoch exhausted");
    }
}

impl ThreadEventChannel {
    pub(in crate::app) fn for_connection_epoch(capacity: usize, epoch: ConnectionEpoch) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        let mut store = ThreadEventStore::new(capacity);
        store.connection_epoch = epoch;
        Self {
            backend_id: None,
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(store)),
            attachment: ThreadEventAttachment::Live,
        }
    }
}

impl ThreadEventStore {
    pub(in crate::app) fn at_connection_epoch(&mut self, epoch: ConnectionEpoch) -> &mut Self {
        if self.connection_epoch != epoch {
            self.operations.reap_connection();
            self.connection_epoch = epoch;
        }
        self
    }
}
