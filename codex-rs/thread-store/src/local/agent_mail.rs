//! Admission witnesses span retained canonical lineage, not just the current model context.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::sync::Arc;
use std::sync::Mutex;

use codex_protocol::ResponseItemId;
use codex_protocol::ThreadId;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::AppendWitness;
use codex_rollout::ResponseItemEnvelope;
use codex_rollout::ReverseJsonlScanner;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutRevision;
use codex_rollout::ScanOutcome;
use codex_rollout::StateDbHandle;
use codex_state::AgentMailState;

use super::LocalThreadStore;
use super::rollout_lineage::RolloutLineage;
use super::rollout_lineage::RolloutLineageSegment;
use super::thread_rollout_resolver;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use crate::is_accepted_agent_mail_id;
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
        self.find_accepted_agent_mail_from_lineage(lineage, ids)
            .await
    }

    async fn find_accepted_agent_mail_from_lineage(
        &self,
        lineage: RolloutLineage,
        ids: &[ResponseItemId],
    ) -> ThreadStoreResult<HashMap<ResponseItemId, ResponseItemEnvelope>> {
        let cacheable = ids.iter().all(is_accepted_agent_mail_id);
        let mut witnesses = HashMap::new();
        if cacheable {
            let recorders: Vec<_> = self
                .live_recorders
                .lock()
                .await
                .values()
                .filter(|entry| {
                    lineage
                        .segments()
                        .iter()
                        .any(|segment| segment.rollout_id == entry.rollout_id)
                })
                .map(|entry| (entry.rollout_id, entry.recorder.clone()))
                .collect();
            for (rollout_id, recorder) in recorders {
                if let Some(witness) = recorder.append_witness().await {
                    witnesses.insert(rollout_id, witness);
                }
            }
        }
        let ids: HashSet<_> = ids.iter().cloned().collect();
        let cache = self.mail_receipts.clone();
        let (stop, keep_running) = tokio::sync::oneshot::channel::<()>();
        let result = tokio::task::spawn_blocking(move || {
            let mut found = HashMap::new();
            for segment in lineage.segments {
                if stop.is_closed() {
                    return Err(mail_history_error("rollout read cancelled"));
                }
                let witness = witnesses.remove(&segment.rollout_id);
                cache.reconcile_segment(segment, witness, cacheable, &ids, &mut found, || {
                    stop.is_closed()
                })?;
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
        let found = self
            .find_accepted_agent_mail_from_lineage(lineage.clone(), &ids)
            .await?;
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

// Cache IDs and byte spans, not payloads. Overflow falls back to canonical scanning.
const MAX_CACHED_SEGMENTS: usize = 64;
const MAX_RECEIPT_RECORDS: usize = 16 * 1024;
const MAX_RECEIPT_ID_BYTES: usize = 1024 * 1024;

#[derive(Clone, Default)]
pub(super) struct ReceiptCache {
    segments: Arc<Mutex<VecDeque<ReceiptCheckpoint>>>,
    #[cfg(test)]
    scanned_records: Arc<std::sync::atomic::AtomicUsize>,
}

struct ReceiptCheckpoint {
    segment: RolloutLineageSegment,
    revision: RolloutRevision,
    witness: Option<AppendWitness>,
    prefix: ValidatedPrefix,
    index: ReceiptIndex,
}

#[derive(Clone, Copy)]
struct ValidatedPrefix {
    end: u64,
    mode: ThreadHistoryMode,
    last_ordinal: Option<u64>,
}

#[derive(Default)]
struct ReceiptIndex {
    entries: HashMap<String, Vec<RecordSpan>>,
    record_count: usize,
    id_bytes: usize,
}

#[derive(Clone, Copy)]
struct RecordSpan {
    offset: u64,
    len: usize,
}

impl ReceiptCache {
    fn take(&self, segment: &RolloutLineageSegment) -> Option<ReceiptCheckpoint> {
        let mut segments = self
            .segments
            .lock()
            .expect("mail receipt cache lock poisoned");
        let index = segments
            .iter()
            .position(|checkpoint| checkpoint.segment == *segment)?;
        segments.remove(index)
    }

    fn save(&self, checkpoint: ReceiptCheckpoint) {
        let mut segments = self
            .segments
            .lock()
            .expect("mail receipt cache lock poisoned");
        segments.retain(|entry| entry.segment != checkpoint.segment);
        segments.push_front(checkpoint);
        segments.truncate(MAX_CACHED_SEGMENTS);
    }

    fn reconcile_segment(
        &self,
        segment: RolloutLineageSegment,
        witness: Option<AppendWitness>,
        cacheable: bool,
        ids: &HashSet<ResponseItemId>,
        found: &mut HashMap<ResponseItemId, ResponseItemEnvelope>,
        is_cancelled: impl Fn() -> bool,
    ) -> ThreadStoreResult<()> {
        let revision = cacheable
            .then(|| RolloutRevision::read(&segment.rollout_path))
            .flatten();
        let witness = witness.filter(|witness| {
            revision
                .as_ref()
                .is_some_and(|revision| witness.matches(revision))
        });
        let checkpoint = revision.as_ref().and_then(|revision| {
            self.take(&segment).filter(|checkpoint| {
                checkpoint.revision == *revision
                    || witness.as_ref().is_some_and(|witness| {
                        checkpoint
                            .witness
                            .as_ref()
                            .is_some_and(|prior| witness.extends(prior))
                    })
            })
        });
        if let (Some(revision), Some(mut checkpoint)) = (&revision, checkpoint) {
            let mut resolved = HashMap::new();
            let mut delta = Some(ReceiptIndex::default());
            let before = checkpoint.revision == *revision || segment.end.is_some();
            let prefix = if before {
                checkpoint.prefix
            } else {
                // Only an observed append can reuse an active segment's validated prefix.
                // The new suffix still has to prove its records, ordinals and retry bytes.
                self.scan_segment(
                    &segment,
                    Some(checkpoint.prefix),
                    &is_cancelled,
                    |envelope, span| collect_record(&mut delta, &mut resolved, ids, envelope, span),
                )?
            };
            let read_indexed_record =
                checkpoint
                    .index
                    .collect(&segment, ids, &mut resolved, &is_cancelled)?;
            if (before && !read_indexed_record)
                || RolloutRevision::read(&segment.rollout_path).as_ref() == Some(revision)
            {
                for envelope in resolved.into_values() {
                    retain_accepted_agent_mail(found, ids, envelope)?;
                }
                if let Some(delta) = delta
                    && checkpoint.index.extend(delta)
                {
                    checkpoint.prefix = prefix;
                    checkpoint.revision = revision.clone();
                    checkpoint.witness = witness;
                    self.save(checkpoint);
                }
                return Ok(());
            }
            // A seal or external change raced the read. A mixed revision is not a checkpoint.
        }

        let mut index = revision.as_ref().map(|_| ReceiptIndex::default());
        let prefix = self.scan_segment(&segment, None, is_cancelled, |envelope, span| {
            collect_record(&mut index, found, ids, envelope, span)
        })?;
        if let (Some(index), Some(revision)) = (index, revision)
            && RolloutRevision::read(&segment.rollout_path).as_ref() == Some(&revision)
        {
            self.save(ReceiptCheckpoint {
                segment,
                revision,
                witness,
                prefix,
                index,
            });
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn scanned_records(&self) -> usize {
        self.scanned_records
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

fn collect_record(
    index: &mut Option<ReceiptIndex>,
    found: &mut HashMap<ResponseItemId, ResponseItemEnvelope>,
    ids: &HashSet<ResponseItemId>,
    envelope: ResponseItemEnvelope,
    span: RecordSpan,
) -> ThreadStoreResult<()> {
    if let Some(index_live) = index
        && let Some(id) = envelope.id().filter(|id| is_accepted_agent_mail_id(id))
        && !index_live.note(id, span)
    {
        *index = None;
    }
    retain_accepted_agent_mail(found, ids, envelope)
}

impl ReceiptIndex {
    fn in_bounds(&self) -> bool {
        self.record_count <= MAX_RECEIPT_RECORDS && self.id_bytes <= MAX_RECEIPT_ID_BYTES
    }

    fn note(&mut self, id: &ResponseItemId, span: RecordSpan) -> bool {
        self.record_count += 1;
        self.id_bytes = self.id_bytes.saturating_add(id.len());
        if !self.in_bounds() {
            return false;
        }
        self.entries.entry(id.to_string()).or_default().push(span);
        true
    }

    fn collect(
        &self,
        segment: &RolloutLineageSegment,
        ids: &HashSet<ResponseItemId>,
        found: &mut HashMap<ResponseItemId, ResponseItemEnvelope>,
        is_cancelled: impl Fn() -> bool,
    ) -> ThreadStoreResult<bool> {
        let mut hits = ids
            .iter()
            .filter_map(|id| Some((id, self.entries.get(id.as_str())?)))
            .peekable();
        if hits.peek().is_none() {
            return Ok(false);
        }
        let mut reader =
            codex_rollout::open_rollout_seekable_reader(&segment.rollout_path, is_cancelled)
                .map_err(mail_history_error)?;
        let mut bytes = Vec::new();
        for (id, spans) in hits {
            for span in spans {
                reader
                    .seek(SeekFrom::Start(span.offset))
                    .map_err(mail_history_error)?;
                bytes.resize(span.len, 0);
                reader.read_exact(&mut bytes).map_err(mail_history_error)?;
                let line = std::str::from_utf8(&bytes).map_err(mail_history_error)?;
                let line = codex_rollout::parse_rollout_line(line).map_err(mail_history_error)?;
                let RolloutItem::ResponseItem(envelope) = line.item else {
                    return Err(mail_history_error(
                        "indexed agent mail is not a response item",
                    ));
                };
                if envelope.id() != Some(id) {
                    return Err(mail_history_error(
                        "indexed agent mail has the wrong identity",
                    ));
                }
                retain_accepted_agent_mail(found, ids, envelope)?;
            }
        }
        Ok(true)
    }

    fn extend(&mut self, newer: Self) -> bool {
        self.record_count += newer.record_count;
        self.id_bytes += newer.id_bytes;
        if !self.in_bounds() {
            return false;
        }
        for (id, mut spans) in newer.entries {
            match self.entries.entry(id) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(spans);
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    spans.append(entry.get_mut());
                    entry.insert(spans);
                }
            }
        }
        true
    }
}

impl ReceiptCache {
    fn scan_segment(
        &self,
        segment: &RolloutLineageSegment,
        prefix: Option<ValidatedPrefix>,
        is_cancelled: impl Fn() -> bool,
        mut record_item: impl FnMut(ResponseItemEnvelope, RecordSpan) -> ThreadStoreResult<()>,
    ) -> ThreadStoreResult<ValidatedPrefix> {
        let mut reader =
            codex_rollout::open_rollout_seekable_reader(&segment.rollout_path, is_cancelled)
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

        let start = prefix.map_or(0, |prefix| prefix.end);
        let mut scanner =
            ReverseJsonlScanner::new_range(reader, start, end).map_err(mail_history_error)?;
        let mut next_ordinal = segment.end.map(|end| end.end_ordinal_exclusive);
        let mut last_ordinal = None;
        let mut valid_ordinals = true;
        let mut oldest_header_mode = None;
        let mut successor = Vec::new();
        while let Some((outcome, record, offset)) = scanner
            .scan_next_rollout_line_with_record()
            .map_err(mail_history_error)?
        {
            #[cfg(test)]
            self.scanned_records
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let line = match outcome {
                ScanOutcome::Parsed(line) => line,
                ScanOutcome::Rejected(error)
                    if error.is_eof() && retried_record_prefix(record, &successor) =>
                {
                    // Reopening can newline-terminate a failed write before retrying its unchanged
                    // ordinal. Only the complete adjacent retry owns admission.
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
            if successor.is_empty() {
                last_ordinal = line.ordinal;
            }
            successor.clear();
            successor.extend_from_slice(record);
            valid_ordinals &= line.ordinal.is_some_and(|ordinal| {
                next_ordinal.is_none_or(|next| ordinal.checked_add(1) == Some(next))
            });
            next_ordinal = line.ordinal;
            // SessionMeta patches are appended records, not new segment headers.
            oldest_header_mode = match &line.item {
                RolloutItem::SessionMeta(meta) => Some(meta.meta.history_mode),
                _ => None,
            };
            if let RolloutItem::ResponseItem(envelope) = line.item {
                record_item(
                    envelope,
                    RecordSpan {
                        offset,
                        len: record.len(),
                    },
                )?;
            }
        }

        let (mode, expected_start) = match prefix {
            Some(prefix) => {
                if successor.is_empty() {
                    return Ok(ValidatedPrefix { end, ..prefix });
                }
                (
                    prefix.mode,
                    prefix
                        .last_ordinal
                        .and_then(|ordinal| ordinal.checked_add(1)),
                )
            }
            None => {
                let Some(mode) = oldest_header_mode else {
                    return Err(mail_history_error("missing canonical lineage header"));
                };
                (mode, segment.start_ordinal.checked_sub(1))
            }
        };
        if mode == ThreadHistoryMode::Paginated
            && (!valid_ordinals || next_ordinal != expected_start)
        {
            return Err(mail_history_error("incomplete canonical ordinal range"));
        }
        Ok(ValidatedPrefix {
            end,
            mode,
            last_ordinal,
        })
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
