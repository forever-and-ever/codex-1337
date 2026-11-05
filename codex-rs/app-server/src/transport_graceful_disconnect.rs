use super::ConnectionId;
use super::OutboundConnectionState;
use super::QueuedOutgoingMessage;
use std::collections::HashMap;
use tokio::sync::oneshot;
use tokio::task::JoinSet;

pub(super) fn flush_connections(
    connections: &HashMap<ConnectionId, OutboundConnectionState>,
    write_complete_tx: oneshot::Sender<()>,
) {
    let mut writers = JoinSet::new();
    for connection in connections.values() {
        let writer = connection.writer.clone();
        writers.spawn(async move {
            let (write_complete_tx, written) = oneshot::channel();
            if writer
                .send(QueuedOutgoingMessage::GracefulClose { write_complete_tx })
                .await
                .is_ok()
            {
                let _ = written.await;
            }
        });
    }
    // Keep the router available for forced cancellation while writers flush.
    tokio::spawn(async move {
        while writers.join_next().await.is_some() {}
        let _ = write_complete_tx.send(());
    });
}
