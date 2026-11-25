use std::io;
use std::mem::take;

use codex_protocol::protocol::SessionMeta;
use tokio::sync::oneshot;
use tracing::warn;

use super::JsonlWriter;
use super::RolloutCmd;
use super::RolloutRecorder;
use super::RolloutWriterState;
use crate::rollout_sink::RolloutSink;

/// Ordinal bounds from the initial session header, not subsequent metadata revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionOrdinals {
    pub initial_ordinal: u64,
    pub subagent_history_start_ordinal: Option<u64>,
}

impl From<&SessionMeta> for ProjectionOrdinals {
    fn from(meta: &SessionMeta) -> Self {
        Self {
            initial_ordinal: meta
                .history_base
                .map_or(0, |base| base.end_ordinal_exclusive),
            subagent_history_start_ordinal: meta.subagent_history_start_ordinal,
        }
    }
}

/// Logical watermark observed after a successful recorder barrier. When present, the bytes at
/// its end are the exact framed JSONL suffix appended by this open writer.
pub struct RolloutCheckpoint {
    end_byte_offset: u64,
    ordinals: ProjectionOrdinals,
    suffix: Vec<u8>,
}

impl RolloutCheckpoint {
    pub fn end_byte_offset(&self) -> u64 {
        self.end_byte_offset
    }

    pub fn ordinals(&self) -> ProjectionOrdinals {
        self.ordinals
    }

    /// Returns the suffix following a checkpoint it covers, or leaves replay to the caller.
    pub fn into_suffix(self, start: u64) -> Option<Vec<u8>> {
        let count = usize::try_from(self.end_byte_offset.checked_sub(start)?).ok()?;
        let skip = self.suffix.len().checked_sub(count)?;
        let mut bytes = self.suffix;
        if skip > 0 {
            bytes.drain(..skip);
        }
        Some(bytes)
    }
}

impl RolloutRecorder {
    pub async fn persist_with_checkpoint(&self) -> io::Result<Option<RolloutCheckpoint>> {
        self.checkpoint_barrier("persist", |ack| RolloutCmd::Persist { ack })
            .await
    }

    pub async fn flush_with_checkpoint(&self) -> io::Result<Option<RolloutCheckpoint>> {
        self.checkpoint_barrier("flush", |ack| RolloutCmd::Flush { ack })
            .await
    }

    async fn checkpoint_barrier(
        &self,
        operation: &str,
        command: impl FnOnce(oneshot::Sender<io::Result<Option<RolloutCheckpoint>>>) -> RolloutCmd,
    ) -> io::Result<Option<RolloutCheckpoint>> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(command(tx)).await.map_err(|err| {
            self.writer_task.terminal_failure().unwrap_or_else(|| {
                io::Error::other(format!("failed to queue rollout {operation}: {err}"))
            })
        })?;
        rx.await.map_err(|err| {
            self.writer_task.terminal_failure().unwrap_or_else(|| {
                io::Error::other(format!("failed waiting for rollout {operation}: {err}"))
            })
        })?
    }
}

impl RolloutWriterState {
    pub(super) async fn checkpoint(&mut self) -> Option<RolloutCheckpoint> {
        let ordinals = self.ordinal_state.projection_ordinals()?;
        let writer = self.writer.as_mut()?;
        let suffix = writer.suffix.as_mut()?;
        match writer.sink.flushed_logical_len(&self.rollout_path).await {
            Ok(Some(end_byte_offset)) => Some(RolloutCheckpoint {
                end_byte_offset,
                ordinals,
                suffix: take(suffix),
            }),
            Ok(None) => {
                writer.suffix = None;
                None
            }
            Err(err) => {
                writer.suffix = None;
                warn!(rollout = %self.rollout_path.display(), "failed to get rollout watermark: {err}");
                None
            }
        }
    }
}

const MAX_PROJECTION_SUFFIX_BYTES: usize = 1024 * 1024;

impl JsonlWriter {
    pub(super) fn new(sink: RolloutSink) -> Self {
        Self {
            sink,
            suffix: Some(Vec::new()),
        }
    }

    pub(super) fn capture_projection_suffix(&mut self, ordinal: Option<u64>, durable: &[u8]) {
        if ordinal.is_some()
            && self.sink.is_framed()
            && let Some(suffix) = self.suffix.as_mut()
        {
            if suffix.len() + durable.len() > MAX_PROJECTION_SUFFIX_BYTES {
                suffix.clear();
            }
            if durable.len() <= MAX_PROJECTION_SUFFIX_BYTES {
                suffix.extend_from_slice(durable);
            }
        }
    }
}
