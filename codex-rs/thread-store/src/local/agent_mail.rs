//! Admission witnesses span retained canonical lineage, not just the current model context.

use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;

use codex_protocol::ResponseItemId;
use codex_protocol::ThreadId;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::ResponseItemEnvelope;
use codex_rollout::ReverseJsonlScanner;
use codex_rollout::RolloutItem;
use codex_rollout::ScanOutcome;
use codex_rollout::StateDbHandle;
use codex_state::AgentMailState;

use super::LocalThreadStore;
use super::rollout_lineage::RolloutLineage;
use super::rollout_lineage::RolloutLineageSegment;
use super::thread_rollout_resolver;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use crate::live_thread::agent_mail::retain_accepted_agent_mail;
use crate::validate_accepted_agent_mail;

impl LocalThreadStore {
    pub(crate) async fn find_accepted_agent_mail(
        &self,
        thread_id: ThreadId,
        ids: &[ResponseItemId],
    ) -> ThreadStoreResult<HashMap<ResponseItemId, ResponseItemEnvelope>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let path = match self.live_rollout_path(thread_id).await {
            Ok(path) => path,
            Err(ThreadStoreError::ThreadNotFound { .. }) => {
                thread_rollout_resolver::resolve_current_including_archived(self, thread_id)
                    .await?
                    .ok_or(ThreadStoreError::ThreadNotFound { thread_id })?
                    .path
            }
            Err(error) => return Err(error),
        };
        let meta = codex_rollout::read_session_meta_line(&path)
            .await
            .map_err(mail_history_error)?;
        if meta.meta.id != thread_id {
            return Err(mail_history_error("rollout belongs to another thread"));
        }
        let lineage = if meta.meta.history_mode == ThreadHistoryMode::Paginated {
            self.resolve_rollout_lineage(thread_id, Some(path)).await?
        } else {
            RolloutLineage {
                segments: vec![RolloutLineageSegment {
                    rollout_id: thread_id,
                    rollout_path: path,
                    start_ordinal: 1,
                    end: None,
                }],
            }
        };
        Self::find_accepted_agent_mail_from_lineage(lineage, ids).await
    }

    pub(super) async fn find_accepted_agent_mail_from_lineage(
        lineage: RolloutLineage,
        ids: &[ResponseItemId],
    ) -> ThreadStoreResult<HashMap<ResponseItemId, ResponseItemEnvelope>> {
        let ids: HashSet<_> = ids.iter().cloned().collect();
        tokio::task::spawn_blocking(move || {
            let mut found = HashMap::new();
            for segment in lineage.segments() {
                let mut reader = codex_rollout::open_rollout_seekable_reader(&segment.rollout_path)
                    .map_err(mail_history_error)?;
                let end = match segment.end {
                    Some(end) => end.end_byte_offset,
                    None => reader.seek(SeekFrom::End(0)).map_err(mail_history_error)?,
                };
                if end == 0 {
                    return Err(mail_history_error("empty canonical lineage segment"));
                }
                reader
                    .seek(SeekFrom::Start(end - 1))
                    .map_err(mail_history_error)?;
                let mut last_byte = [0];
                reader
                    .read_exact(&mut last_byte)
                    .map_err(mail_history_error)?;
                if last_byte != [b'\n'] {
                    return Err(mail_history_error(
                        "incomplete canonical record at lineage cutoff",
                    ));
                }
                let mut scanner =
                    ReverseJsonlScanner::new_at(reader, end).map_err(mail_history_error)?;
                let mut next_ordinal = segment.end.map(|end| end.end_ordinal_exclusive);
                let mut valid_ordinals = true;
                let mut saw_header = false;
                while let Some(outcome) = scanner
                    .scan_next_rollout_line()
                    .map_err(mail_history_error)?
                {
                    let line = match outcome {
                        ScanOutcome::Parsed(line) => line,
                        ScanOutcome::Rejected(error) => return Err(mail_history_error(error)),
                    };
                    if saw_header {
                        return Err(mail_history_error(
                            "records precede canonical session metadata",
                        ));
                    }
                    valid_ordinals &= line.ordinal.is_some_and(|ordinal| {
                        next_ordinal.is_none_or(|next| ordinal.checked_add(1) == Some(next))
                    });
                    next_ordinal = line.ordinal;
                    match line.item {
                        RolloutItem::SessionMeta(meta) => {
                            if meta.meta.history_mode == ThreadHistoryMode::Paginated
                                && (!valid_ordinals
                                    || next_ordinal != segment.start_ordinal.checked_sub(1))
                            {
                                return Err(mail_history_error(
                                    "incomplete canonical ordinal range",
                                ));
                            }
                            saw_header = true;
                        }
                        RolloutItem::ResponseItem(envelope) => {
                            retain_accepted_agent_mail(&mut found, &ids, envelope)?;
                        }
                        _ => {}
                    }
                }
                if !saw_header {
                    return Err(mail_history_error("incomplete canonical lineage segment"));
                }
            }
            Ok(found)
        })
        .await
        .map_err(mail_history_error)?
    }

    /// The caller owns the unloaded recipient's lifecycle and writer locks through cutover.
    pub(super) async fn acknowledge_agent_mail_before_revert(
        &self,
        state_db: &StateDbHandle,
        thread_id: ThreadId,
        lineage: &RolloutLineage,
    ) -> ThreadStoreResult<()> {
        let pending = state_db
            .thread_queue()
            .pending_agent_mail(thread_id)
            .await
            .map_err(mail_history_error)?;
        if pending.is_empty() {
            return Ok(());
        }
        let ids: Vec<_> = pending.iter().map(|record| record.id.clone()).collect();
        let found = Self::find_accepted_agent_mail_from_lineage(lineage.clone(), &ids).await?;
        for record in pending {
            let AgentMailState::Pending(payload) = record.state else {
                unreachable!()
            };
            let mut communication: InterAgentCommunication =
                serde_json::from_str(&payload).map_err(mail_history_error)?;
            communication.id = Some(record.id.clone());
            if let Some(envelope) = found.get(&record.id) {
                validate_accepted_agent_mail(envelope, &communication)?;
                // This monotonic acknowledgment must precede discarding the canonical witness.
                // Rows accepted after our snapshot cannot be admitted under our writer lock.
                state_db
                    .thread_queue()
                    .admit_agent_mail(thread_id, &record.id)
                    .await
                    .map_err(mail_history_error)?;
            }
        }
        Ok(())
    }
}

fn mail_history_error(error: impl std::fmt::Display) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: format!(
            "cannot reconcile canonical accepted agent mail: {error}; preserve storage and unload/reload or resume the recipient"
        ),
    }
}
