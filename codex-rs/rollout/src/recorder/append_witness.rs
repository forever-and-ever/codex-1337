//! Binds a validated rollout prefix to one continuously observed append-only writer.

use std::fs::Metadata;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::oneshot;

use super::RolloutCmd;
use super::RolloutRecorder;
use super::RolloutWriterState;

/// The selected physical representation, including same-size changes to its inode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutRevision {
    compressed: bool,
    device: u64,
    inode: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl RolloutRevision {
    /// Snapshot the reader's current plain-before-compressed selection. Blocking.
    pub fn read(path: &Path) -> Option<Self> {
        let (path, metadata) = crate::existing_rollout_with_metadata_sync(path)?;
        Self::from_metadata(
            &metadata,
            crate::compression::is_compressed_rollout_path(&path),
        )
    }

    pub(crate) fn from_metadata(metadata: &Metadata, compressed: bool) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            Some(Self {
                compressed,
                device: metadata.dev(),
                inode: metadata.ino(),
                len: metadata.len(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (metadata, compressed);
            None
        }
    }

    fn continues(&self, prior: &Self) -> bool {
        self == prior
            || (self.compressed == prior.compressed
                && self.device == prior.device
                && self.inode == prior.inode
                && self.len > prior.len)
    }
}

/// Taken from an open writer, never from a projection. A failed write or reopening breaks its epoch.
#[derive(Clone, Debug)]
pub struct AppendWitness {
    revision: RolloutRevision,
    epoch: Arc<()>,
}

impl AppendWitness {
    pub fn matches(&self, revision: &RolloutRevision) -> bool {
        self.revision == *revision
    }

    pub fn extends(&self, prior: &Self) -> bool {
        Arc::ptr_eq(&self.epoch, &prior.epoch)
    }

    pub(crate) fn observe(slot: &mut Option<Self>, revision: RolloutRevision) -> Self {
        if let Some(witness) = slot
            && witness.matches(&revision)
        {
            return witness.clone();
        }
        let witness = Self {
            revision,
            epoch: Arc::new(()),
        };
        *slot = Some(witness.clone());
        witness
    }

    pub(crate) fn advance(self, revision: RolloutRevision) -> Option<Self> {
        revision.continues(&self.revision).then_some(Self {
            revision,
            epoch: self.epoch,
        })
    }
}

impl RolloutRecorder {
    /// Observe the open file between writer operations, including idle frame seals.
    pub async fn append_witness(&self) -> Option<AppendWitness> {
        let (ack, response) = oneshot::channel();
        self.tx
            .send(RolloutCmd::ObserveAppendWitness { ack })
            .await
            .ok()?;
        response.await.ok().flatten()
    }
}

impl RolloutWriterState {
    pub(super) async fn observe_append_witness(&mut self) -> Option<AppendWitness> {
        self.writer.as_mut()?.sink.observe_append_witness().await
    }

    pub(super) async fn write_pending_items_with_witness(&mut self) -> std::io::Result<()> {
        if self.pending_items.is_empty() {
            return Ok(());
        }
        let before = match self.writer.as_mut() {
            Some(writer) => writer.sink.begin_append().await,
            None => None,
        };
        self.write_pending_items_once().await?;
        if let Some(writer) = self.writer.as_mut() {
            writer.sink.finish_append(before).await;
        }
        Ok(())
    }
}
