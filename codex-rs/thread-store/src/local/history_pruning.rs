//! Evicts the history projections of threads unused for 14 days and reclaims their database
//! pages.
//!
//! The projection is a rebuildable view of rollouts, keyed by rollout ID. Each rollout's row in
//! `thread_history_projection_state` is in one of three states:
//!
//! - absent: never built, or evicted. Evicting deletes the projected rows and the state row in
//!   one transaction, which resets catch-up to offset 0.
//! - `rebuilding`: a pass from offset 0 is running or was interrupted.
//! - current through its offset.
//!
//! Every paginated read first calls [`ensure_projected`] on each lineage segment. That builds an
//! absent projection, or finishes an interrupted one, before the read, so a rebuild in progress is
//! never read as an empty or truncated history. Rebuilds use the existing catch-up path,
//! `materialize_to_sqlite`, under a per-rollout lock that every pass takes.
//!
//! A projection's use time is `accessed_at_ms`. Projected writes (turns) set it, and reads and
//! resumes advance it at most hourly. Rows that predate the column have 0 and fall back to the
//! thread's `updated_at`. A projection is evicted only after 14 days without use, and never while
//! its thread is loaded in this process or holds its cross-process writer lock elsewhere.
//!
//! Evicted pages are reclaimed by bounded `PRAGMA incremental_vacuum` chunks. A chunk searches the
//! freelist for every page it frees, so a freelist of millions of pages slows chunks to a crawl;
//! while freed pages are at least half the file and live data is at most 6 GiB, the database is
//! instead repacked by a full VACUUM, which also converts one still in `auto_vacuum=NONE`. A
//! repack runs only in a quiet window, at most daily.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

use codex_protocol::ThreadId;
use codex_state::SqlitePoolWriteExt;
use tokio::sync::Mutex;
use tokio::sync::OwnedMutexGuard;
use tracing::info;
use tracing::warn;

use super::LocalThreadStore;
use super::rollout_lineage::AncestorReads;
use super::thread_history::RolloutProjectionState;
use super::thread_history::projection_state;
use super::thread_history_materialization::materialize_to_sqlite_locked;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

const EVICT_AFTER: Duration = Duration::from_hours(14 * 24);
/// Reads and resumes advance a projection's use time at most this often.
const TOUCH_GRANULARITY: Duration = Duration::from_hours(1);
const FIRST_PASS_DELAY: Duration = Duration::from_mins(10);
const PASS_INTERVAL: Duration = Duration::from_hours(6);
/// Space between evictions, each one transaction, so projection writers interleave.
const EVICTION_PAUSE: Duration = Duration::from_millis(25);
/// No projection write for this long before and immediately before a repack's VACUUM.
const QUIET_WINDOW: Duration = Duration::from_mins(5);
const REPACK_RETRY: Duration = Duration::from_hours(24);
const REPACK_MAX_LIVE_BYTES: i64 = 6 << 30;
const REPACK_STEP: &str = "repack";
/// Frees up to 2048 pages (8 MiB at the default page size) per write transaction.
const VACUUM_CHUNK: &str = "PRAGMA incremental_vacuum(2048)";
const VACUUM_CHUNK_PAUSE: Duration = Duration::from_millis(100);
const VACUUM_PASS_BUDGET: Duration = Duration::from_secs(10);
const AUTO_VACUUM_INCREMENTAL: i64 = 2;

/// Serializes projection passes per rollout and remembers this process's last projected write.
#[derive(Default)]
pub(super) struct ProjectionWarden {
    // Locks stay after use: removing one while another caller is about to acquire it could let
    // two passes over one rollout run at once.
    by_rollout: Mutex<HashMap<ThreadId, Arc<Mutex<()>>>>,
    last_write_ms: AtomicI64,
}

impl ProjectionWarden {
    pub(super) async fn lock(&self, rollout_id: ThreadId) -> OwnedMutexGuard<()> {
        let lock = Arc::clone(self.by_rollout.lock().await.entry(rollout_id).or_default());
        lock.lock_owned().await
    }

    pub(super) fn record_write(&self) {
        self.last_write_ms.store(now_ms(), Ordering::Relaxed);
    }
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Marks a projection built from offset 0 as complete. The caller holds its projection lock.
pub(super) async fn finish_rebuild(
    store: &LocalThreadStore,
    rollout_id: ThreadId,
    next_offset: u64,
) -> ThreadStoreResult<()> {
    let pool = store.thread_history_db().await?;
    let next_offset = i64::try_from(next_offset).map_err(pruning_error)?;
    sqlx::query(
        "UPDATE thread_history_projection_state SET rebuilding = 0
         WHERE thread_id = ? AND next_rollout_byte_offset = ? AND rebuilding = 1",
    )
    .bind(rollout_id.to_string())
    .bind(next_offset)
    .execute(pool)
    .await
    .map_err(pruning_error)?;
    Ok(())
}

/// Finishes an evicted or interrupted projection before the caller queries it.
pub(super) async fn ensure_projected(
    store: &LocalThreadStore,
    rollout_id: ThreadId,
    rollout_path: &std::path::Path,
    reads: &AncestorReads,
) -> ThreadStoreResult<()> {
    if store.state_db.is_none() {
        return Ok(());
    }
    let _projection = store.projection_warden.lock(rollout_id).await;
    let covers_request = |state: &RolloutProjectionState| match reads.cutoff(rollout_path) {
        Some(end) => state.next_byte_offset >= end.end_byte_offset,
        None => !state.rebuilding,
    };
    if let Some(state) = projection_state(store, rollout_id).await?
        && covers_request(&state)
    {
        return touch_locked(store, rollout_id, &state).await;
    }
    let started = Instant::now();
    let built = materialize_to_sqlite_locked(store, rollout_id, rollout_path, reads).await;
    // The checkpoint covers inherited prefixes even when the suffix could not be projected.
    let state = projection_state(store, rollout_id).await?;
    match (built, state) {
        (_, Some(state)) if covers_request(&state) => {
            info!(
                %rollout_id,
                elapsed_ms = millis(started.elapsed()),
                "rebuilt thread history projection"
            );
            touch_locked(store, rollout_id, &state).await
        }
        (Ok(()), None) if !reads.tracks(rollout_path) => Ok(()),
        (Err(err), _) => Err(err),
        (Ok(()), _) => Err(ThreadStoreError::Internal {
            message: format!(
                "thread history projection for {rollout_id} did not reach the requested end"
            ),
        }),
    }
}

/// Records that a rollout's projection was opened, if it has one.
pub(super) async fn touch(store: &LocalThreadStore, rollout_id: ThreadId) -> ThreadStoreResult<()> {
    if store.state_db.is_none() {
        return Ok(());
    }
    let _projection = store.projection_warden.lock(rollout_id).await;
    match projection_state(store, rollout_id).await? {
        Some(state) => touch_locked(store, rollout_id, &state).await,
        None => Ok(()),
    }
}

async fn touch_locked(
    store: &LocalThreadStore,
    rollout_id: ThreadId,
    state: &RolloutProjectionState,
) -> ThreadStoreResult<()> {
    let now = now_ms();
    if now.saturating_sub(state.accessed_at_ms) < millis(TOUCH_GRANULARITY) {
        return Ok(());
    }
    let pool = store.thread_history_db().await?;
    sqlx::query(
        "UPDATE thread_history_projection_state SET accessed_at_ms = ? WHERE thread_id = ?",
    )
    .bind(now)
    .bind(rollout_id.to_string())
    .execute(pool)
    .await
    .map_err(pruning_error)?;
    Ok(())
}

impl LocalThreadStore {
    /// Prunes unused history projections in the background: first after a delay, then
    /// periodically. Only one process per projection database prunes at a time.
    pub fn spawn_history_pruning(&self) {
        if self.state_db.is_none() {
            return;
        }
        let store = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_PASS_DELAY).await;
            loop {
                if let Err(err) = prune(&store).await {
                    warn!("thread history pruning failed: {err}");
                }
                tokio::time::sleep(PASS_INTERVAL).await;
            }
        });
    }
}

/// One pruning pass: evict, then reclaim. Returns the number of evicted projections.
pub(super) async fn prune(store: &LocalThreadStore) -> ThreadStoreResult<usize> {
    let db_path = store.config.sqlite.thread_history_db_path();
    if !tokio::fs::try_exists(db_path.as_path())
        .await
        .map_err(pruning_error)?
    {
        return Ok(0);
    }
    let Some(_pruner) = elect_pruner(db_path.with_extension("pruning.lock"))? else {
        return Ok(0);
    };
    let evicted = evict_unused(store, now_ms().saturating_sub(millis(EVICT_AFTER))).await?;
    reclaim(store, db_path).await?;
    Ok(evicted)
}

fn elect_pruner(path: PathBuf) -> ThreadStoreResult<Option<File>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(pruning_error)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(err)) => Err(pruning_error(err)),
    }
}

/// Evicts every projection unused since `cutoff_ms` whose thread is not loaded.
pub(super) async fn evict_unused(
    store: &LocalThreadStore,
    cutoff_ms: i64,
) -> ThreadStoreResult<usize> {
    let pool = store.thread_history_db().await?;
    let candidates = sqlx::query_as::<_, (String, i64)>(
        "SELECT thread_id, accessed_at_ms FROM thread_history_projection_state \
         WHERE accessed_at_ms < ? ORDER BY accessed_at_ms",
    )
    .bind(cutoff_ms)
    .fetch_all(pool)
    .await
    .map_err(pruning_error)?;
    let started = Instant::now();
    let mut evicted = 0;
    for (rollout_id, accessed_at_ms) in candidates {
        let Ok(rollout_id) = ThreadId::from_string(&rollout_id) else {
            continue;
        };
        // A rollout whose ID names a thread has that thread as owner; a revert's rollout does not.
        let owner = match &store.state_db {
            Some(state_db) => state_db
                .get_thread(rollout_id)
                .await
                .map_err(pruning_error)?,
            None => None,
        };
        if accessed_at_ms == 0
            && owner
                .as_ref()
                .is_some_and(|thread| thread.updated_at.timestamp_millis() >= cutoff_ms)
        {
            continue;
        }
        if evict(store, rollout_id, owner.is_some(), cutoff_ms).await? {
            evicted += 1;
        }
        tokio::time::sleep(EVICTION_PAUSE).await;
    }
    if evicted > 0 {
        info!(
            evicted,
            elapsed_ms = millis(started.elapsed()),
            "evicted unused thread history projections"
        );
    }
    Ok(evicted)
}

/// Deletes one projection and its catch-up progress in one transaction, unless its thread is
/// loaded here or elsewhere or it was used since `cutoff_ms`.
async fn evict(
    store: &LocalThreadStore,
    rollout_id: ThreadId,
    owned_by_thread: bool,
    cutoff_ms: i64,
) -> ThreadStoreResult<bool> {
    // A resume of this thread waits here rather than failing on the writer lock below.
    let _live_writer = store.live_writer_locks.lock(rollout_id).await;
    if store
        .live_recorders
        .lock()
        .await
        .iter()
        .any(|(thread_id, entry)| *thread_id == rollout_id || entry.rollout_id == rollout_id)
    {
        return Ok(false);
    }
    let _writer_lock = if owned_by_thread {
        let coordinator = Arc::clone(&store.writer_lock_coordinator);
        match tokio::task::spawn_blocking(move || coordinator.acquire(rollout_id))
            .await
            .map_err(pruning_error)?
        {
            Ok(guard) => Some(guard),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(err) => return Err(pruning_error(err)),
        }
    } else {
        None
    };
    let _projection = store.projection_warden.lock(rollout_id).await;
    let pool = store.thread_history_db().await?;
    let mut transaction = pool.begin_immediate().await.map_err(pruning_error)?;
    let rollout_id = rollout_id.to_string();
    let reset = sqlx::query(
        "DELETE FROM thread_history_projection_state WHERE thread_id = ? AND accessed_at_ms < ?",
    )
    .bind(rollout_id.as_str())
    .bind(cutoff_ms)
    .execute(&mut *transaction)
    .await
    .map_err(pruning_error)?;
    if reset.rows_affected() == 0 {
        return Ok(false);
    }
    for delete in [
        "DELETE FROM thread_items WHERE thread_id = ?",
        "DELETE FROM thread_turns WHERE thread_id = ?",
        "DELETE FROM thread_realtime_items WHERE thread_id = ?",
    ] {
        sqlx::query(delete)
            .bind(rollout_id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(pruning_error)?;
    }
    transaction.commit().await.map_err(pruning_error)?;
    Ok(true)
}

/// Frees evicted pages: a repack while they are at least half the file, else bounded incremental
/// vacuums.
async fn reclaim(store: &LocalThreadStore, db_path: PathBuf) -> ThreadStoreResult<()> {
    let pool = store.thread_history_db().await?;
    let pages = page_census(pool).await?;
    if pages.worth_repacking() {
        // The pass's own deletes count as writes, so the window starts after them.
        tokio::time::sleep(QUIET_WINDOW).await;
        return repack(store, db_path).await;
    }
    if pages.auto_vacuum != AUTO_VACUUM_INCREMENTAL {
        return Ok(());
    }
    let started = Instant::now();
    let mut free = pages.freelist;
    while free > 0 && started.elapsed() < VACUUM_PASS_BUDGET {
        sqlx::query(VACUUM_CHUNK)
            .execute(pool)
            .await
            .map_err(pruning_error)?;
        free = page_census(pool).await?.freelist;
        tokio::time::sleep(VACUUM_CHUNK_PAUSE).await;
    }
    sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
        .execute(pool)
        .await
        .map_err(pruning_error)?;
    Ok(())
}

struct PageCensus {
    auto_vacuum: i64,
    page_size: i64,
    page_count: i64,
    freelist: i64,
}

impl PageCensus {
    fn worth_repacking(&self) -> bool {
        self.freelist.saturating_mul(2) >= self.page_count
            && (self.page_count - self.freelist).saturating_mul(self.page_size)
                <= REPACK_MAX_LIVE_BYTES
    }
}

async fn page_census(pool: &sqlx::SqlitePool) -> ThreadStoreResult<PageCensus> {
    let (auto_vacuum, page_size, page_count, freelist) = sqlx::query_as::<_, (i64, i64, i64, i64)>(
        "SELECT auto_vacuum, page_size, page_count, freelist_count \
         FROM pragma_auto_vacuum, pragma_page_size, pragma_page_count, pragma_freelist_count",
    )
    .fetch_one(pool)
    .await
    .map_err(pruning_error)?;
    Ok(PageCensus {
        auto_vacuum,
        page_size,
        page_count,
        freelist,
    })
}

/// Rewrites the projection database without its free pages, in `auto_vacuum=INCREMENTAL`. VACUUM
/// is atomic, so a failure leaves the database as it was; the attempt is recorded first, so
/// attempts are at most daily.
async fn repack(store: &LocalThreadStore, db_path: PathBuf) -> ThreadStoreResult<()> {
    let pool = store.thread_history_db().await?;
    let attempted_at_ms = sqlx::query_scalar::<_, i64>(
        "SELECT attempted_at_ms FROM thread_history_maintenance WHERE step = ?",
    )
    .bind(REPACK_STEP)
    .fetch_optional(pool)
    .await
    .map_err(pruning_error)?;
    if attempted_at_ms
        .is_some_and(|attempted| now_ms().saturating_sub(attempted) < millis(REPACK_RETRY))
        || !quiet(store, &db_path)
    {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO thread_history_maintenance (step, attempted_at_ms) VALUES (?, ?) \
         ON CONFLICT(step) DO UPDATE SET attempted_at_ms = excluded.attempted_at_ms",
    )
    .bind(REPACK_STEP)
    .bind(now_ms())
    .execute(pool)
    .await
    .map_err(pruning_error)?;
    let recorded_at_ms = now_ms();
    // Immediately before the VACUUM: no writer but this attempt record has touched the WAL.
    if !quiet_window_holds(
        store
            .projection_warden
            .last_write_ms
            .load(Ordering::Relaxed),
        wal_modified_ms(&db_path).filter(|modified| *modified > recorded_at_ms),
    ) {
        return Ok(());
    }
    let before = page_census(pool).await?;
    // SQLite runs each pooled connection on its own thread, so the VACUUM blocks no async worker.
    let mut connection = pool.acquire().await.map_err(pruning_error)?;
    let started = Instant::now();
    let result = async {
        sqlx::query("PRAGMA auto_vacuum = INCREMENTAL")
            .execute(&mut *connection)
            .await?;
        sqlx::query("VACUUM").execute(&mut *connection).await?;
        Ok::<_, sqlx::Error>(())
    }
    .await;
    let elapsed_ms = millis(started.elapsed());
    let wal_bytes = std::fs::metadata(wal_path(&db_path)).map_or(0, |metadata| metadata.len());
    match result {
        Ok(()) => {
            let after = page_census(pool).await?;
            info!(
                elapsed_ms,
                wal_bytes,
                bytes_before = before.page_count * before.page_size,
                bytes_after = after.page_count * after.page_size,
                "repacked the thread history database"
            );
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&mut *connection)
                .await
                .map_err(pruning_error)?;
            Ok(())
        }
        Err(err) => {
            warn!(elapsed_ms, "thread history database repack failed: {err}");
            Ok(())
        }
    }
}

/// No projection write in this process, and no write to the database's WAL, for the window.
fn quiet(store: &LocalThreadStore, db_path: &std::path::Path) -> bool {
    quiet_window_holds(
        store
            .projection_warden
            .last_write_ms
            .load(Ordering::Relaxed),
        wal_modified_ms(db_path),
    )
}

fn quiet_window_holds(last_write_ms: i64, wal_modified_ms: Option<i64>) -> bool {
    let quiet_after = now_ms().saturating_sub(millis(QUIET_WINDOW));
    last_write_ms < quiet_after && wal_modified_ms.is_none_or(|modified| modified < quiet_after)
}

fn wal_path(db_path: &std::path::Path) -> PathBuf {
    let mut path = db_path.as_os_str().to_owned();
    path.push("-wal");
    PathBuf::from(path)
}

fn wal_modified_ms(db_path: &std::path::Path) -> Option<i64> {
    let modified = std::fs::metadata(wal_path(db_path)).ok()?.modified().ok()?;
    let elapsed = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    i64::try_from(elapsed.as_millis()).ok()
}

fn pruning_error(err: impl std::fmt::Display) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: format!("thread history pruning: {err}"),
    }
}
