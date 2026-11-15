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
        // Append owns this guard through canonical flush, not just recorder enqueue.
        // Existing-lineage resolution never reacquires writer guards.
        let _writer = self.live_writer_locks.lock(thread_id).await;
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
        let lineage = match meta.meta.history_mode {
            ThreadHistoryMode::Legacy => RolloutLineage::legacy(thread_id, path),
            ThreadHistoryMode::Paginated => {
                self.resolve_rollout_lineage(thread_id, Some(path)).await?
            }
        };
        Self::find_accepted_agent_mail_from_lineage(lineage, ids).await
    }

    pub(super) async fn find_accepted_agent_mail_from_lineage(
        lineage: RolloutLineage,
        ids: &[ResponseItemId],
    ) -> ThreadStoreResult<HashMap<ResponseItemId, ResponseItemEnvelope>> {
        let ids: HashSet<_> = ids.iter().cloned().collect();
        let (stop, keep_running) = tokio::sync::oneshot::channel::<()>();
        let result = tokio::task::spawn_blocking(move || {
            let mut found = HashMap::new();
            for segment in lineage.segments() {
                let mut reader =
                    codex_rollout::open_rollout_seekable_reader(&segment.rollout_path, || {
                        stop.is_closed()
                    })
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
                if last_byte != *b"\n" {
                    return Err(mail_history_error(
                        "incomplete canonical record at lineage cutoff",
                    ));
                }
                let mut scanner =
                    ReverseJsonlScanner::new_at(reader, end).map_err(mail_history_error)?;
                let mut next_ordinal = segment.end.map(|end| end.end_ordinal_exclusive);
                let mut valid_ordinals = true;
                let mut oldest_header_mode = None;
                let mut successor = Vec::new();
                while let Some((outcome, record)) = scanner
                    .scan_next_rollout_line_with_record()
                    .map_err(mail_history_error)?
                {
                    let line = match outcome {
                        ScanOutcome::Parsed(line) => line,
                        ScanOutcome::Rejected(error)
                            if error.is_eof() && retried_record_prefix(record, &successor) =>
                        {
                            // Reopening can newline-terminate a failed write before retrying
                            // its unchanged ordinal. The complete adjacent retry owns admission;
                            // a matching partial attempt adds no record and loses no bytes.
                            tracing::warn!(rollout = %segment.rollout_path.display(),
                                ordinal = ?next_ordinal, bytes = record.len(),
                                "canonical reconciliation verified a truncated retry prefix");
                            continue;
                        }
                        ScanOutcome::Rejected(error) => {
                            return Err(mail_history_error(format!(
                                "{} before ordinal {:?}: {error}",
                                segment.rollout_path.display(),
                                next_ordinal
                            )));
                        }
                    };
                    successor.clear();
                    successor.extend_from_slice(record);
                    valid_ordinals &= line.ordinal.is_some_and(|ordinal| {
                        next_ordinal.is_none_or(|next| ordinal.checked_add(1) == Some(next))
                    });
                    next_ordinal = line.ordinal;
                    // SessionMeta patches are appended records, not new segment headers.
                    // Only the oldest physical record must be the canonical header.
                    oldest_header_mode = match &line.item {
                        RolloutItem::SessionMeta(meta) => Some(meta.meta.history_mode),
                        _ => None,
                    };
                    if let RolloutItem::ResponseItem(envelope) = line.item {
                        retain_accepted_agent_mail(&mut found, &ids, envelope)?;
                    }
                }
                let Some(mode) = oldest_header_mode else {
                    return Err(mail_history_error("missing canonical lineage header"));
                };
                if mode == ThreadHistoryMode::Paginated
                    && (!valid_ordinals || next_ordinal != segment.start_ordinal.checked_sub(1))
                {
                    return Err(mail_history_error("incomplete canonical ordinal range"));
                }
            }
            Ok(found)
        })
        .await
        .map_err(mail_history_error)?;
        drop(keep_running);
        result
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

fn retried_record_prefix(partial: &[u8], complete: &[u8]) -> bool {
    fn without_timestamp(record: &[u8]) -> Option<&[u8]> {
        let value = record.strip_prefix(b"{\"timestamp\":")?;
        let mut timestamp = serde_json::Deserializer::from_slice(value).into_iter::<String>();
        timestamp.next()?.ok()?;
        let rest = &value[timestamp.byte_offset()..];
        // Require a complete numeric ordinal and item discriminator, not a shared JSON header.
        let value = rest.strip_prefix(b",\"ordinal\":")?;
        let mut ordinal = serde_json::Deserializer::from_slice(value).into_iter::<u64>();
        ordinal.next()?.ok()?;
        let value = value[ordinal.byte_offset()..].strip_prefix(b",\"type\":")?;
        let mut kind = serde_json::Deserializer::from_slice(value).into_iter::<String>();
        kind.next()?.ok()?;
        value[kind.byte_offset()..].strip_prefix(b",\"payload\":")?;
        Some(rest)
    }
    let Some(partial) = without_timestamp(partial) else {
        return false;
    };
    let Some(complete) = without_timestamp(complete) else {
        return false;
    };
    partial.len() < complete.len() && complete.starts_with(partial)
}

fn mail_history_error(error: impl std::fmt::Display) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: format!(
            "cannot reconcile canonical accepted agent mail: {error}; preserve storage and unload/reload or resume the recipient"
        ),
    }
}
