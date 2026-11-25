use std::collections::HashMap;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use codex_protocol::RolloutId;
use codex_protocol::ThreadId;
use tokio::fs;

use crate::ARCHIVED_SESSIONS_SUBDIR;
use crate::SESSIONS_SUBDIR;
use crate::list::read_exact_session_meta_line;
use crate::rollout_file_name::RolloutFileName;

/// Physical rollout copies and their references, verified for deletion.
pub struct RolloutDeletionIndex {
    rollouts: HashMap<RolloutId, RolloutCopies>,
}

struct RolloutCopies {
    owner: ThreadId,
    copies: Vec<RolloutCopy>,
}

struct RolloutCopy {
    path: PathBuf,
    history_base: Option<RolloutId>,
    attached_parent: Option<ThreadId>,
}

impl RolloutDeletionIndex {
    /// Requires a readable header for every canonical active and archived rollout file.
    ///
    /// A readable copy cannot vouch for another physical file with the same rollout ID.
    pub async fn scan(codex_home: &Path) -> Result<Self> {
        let mut rollouts = HashMap::<_, RolloutCopies>::new();
        for subdir in [SESSIONS_SUBDIR, ARCHIVED_SESSIONS_SUBDIR] {
            let root = codex_home.join(subdir);
            match fs::symlink_metadata(&root).await {
                Ok(_) => {}
                Err(err) if err.kind() == ErrorKind::NotFound => continue,
                Err(err) => return Err(err).with_context(|| format!("inspect {}", root.display())),
            }
            let mut stack = vec![root];
            while let Some(directory) = stack.pop() {
                let directory_context =
                    || format!("scan rollout directory {}", directory.display());
                let mut entries = fs::read_dir(&directory)
                    .await
                    .with_context(directory_context)?;
                while let Some(entry) =
                    entries.next_entry().await.with_context(directory_context)?
                {
                    let path = entry.path();
                    let file_type = entry
                        .file_type()
                        .await
                        .with_context(|| format!("inspect {}", path.display()))?;
                    if file_type.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    let file_name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(RolloutFileName::parse);
                    if file_type.is_symlink() {
                        ensure!(
                            file_name.is_none() && !symlink_target_is_directory(&path).await?,
                            "cannot verify symlinked rollout or subtree: {}",
                            path.display()
                        );
                        continue;
                    }
                    let Some(file_name) = file_name else {
                        continue;
                    };
                    ensure!(
                        file_type.is_file(),
                        "rollout is not a regular file: {}",
                        path.display()
                    );
                    let (owner, copy) = read_copy(path).await?;
                    let copies =
                        rollouts
                            .entry(file_name.rollout_id())
                            .or_insert_with(|| RolloutCopies {
                                owner,
                                copies: Vec::new(),
                            });
                    ensure!(
                        copies.owner == owner,
                        "rollout {} has conflicting owners at {}",
                        file_name.rollout_id(),
                        copy.path.display()
                    );
                    copies.copies.push(copy);
                }
            }
        }
        Ok(Self { rollouts })
    }

    /// Returns every physical copy owned by the thread.
    pub fn rollouts_for_thread(
        &self,
        thread_id: ThreadId,
    ) -> impl Iterator<Item = (RolloutId, &Path)> {
        self.rollouts
            .iter()
            .filter(move |(_, rollout)| rollout.owner == thread_id)
            .flat_map(|(&rollout_id, rollout)| {
                rollout
                    .copies
                    .iter()
                    .map(move |copy| (rollout_id, copy.path.as_path()))
            })
    }

    /// Finds an attached child of any candidate parent.
    pub fn attached_child(&self, parents: &[ThreadId]) -> Option<ThreadId> {
        let parents: HashSet<_> = parents.iter().copied().collect();
        self.rollouts.values().find_map(|rollout| {
            rollout
                .copies
                .iter()
                .any(|copy| {
                    copy.attached_parent
                        .is_some_and(|parent| parents.contains(&parent))
                })
                .then_some(rollout.owner)
        })
    }

    /// Returns the history bases referenced by rollouts outside the deletion set.
    pub fn external_history_bases<'a>(
        &'a self,
        deletion_rollout_ids: &'a HashSet<RolloutId>,
    ) -> impl Iterator<Item = RolloutId> + 'a {
        self.rollouts
            .iter()
            .filter(move |(rollout_id, _)| !deletion_rollout_ids.contains(rollout_id))
            .flat_map(|(&rollout_id, rollout)| {
                rollout
                    .copies
                    .iter()
                    .filter_map(move |copy| copy.history_base.filter(|&base| base != rollout_id))
            })
    }
}

async fn read_copy(path: PathBuf) -> Result<(ThreadId, RolloutCopy)> {
    let meta = read_exact_session_meta_line(&path)
        .await
        .with_context(|| format!("read rollout header {}", path.display()))?
        .meta;
    Ok((
        meta.id,
        RolloutCopy {
            path,
            history_base: meta.history_base.map(|base| base.thread_id),
            attached_parent: meta.forked_from_id.filter(|_| meta.attached_thread),
        },
    ))
}

async fn symlink_target_is_directory(path: &Path) -> Result<bool> {
    match fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("inspect symlink target {}", path.display())),
    }
}
