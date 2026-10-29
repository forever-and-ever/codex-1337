//! Queue integrity findings are authoritative: cached corruption never becomes permission.

use super::DbKind;
use super::DbTelemetry;
use super::RuntimeDbSpec;
use super::SqliteConfig;
use super::telemetry;
use super::validation::QuickCheckOutcome;
use super::validation::quick_check;
use file_id::FileId;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::sync::OnceCell;

#[derive(Clone, Debug, Default)]
pub(super) struct QueueIntegrity(Arc<Mutex<HashMap<FileId, Arc<OnceCell<QuickCheckOutcome>>>>>);

impl SqliteConfig {
    pub(super) async fn check_runtime_integrity(
        &self,
        pool: &SqlitePool,
        path: &Path,
        spec: Option<RuntimeDbSpec>,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<QuickCheckOutcome> {
        // TELOS(authoritative-queue-integrity): Share queue verdicts, not upstream's attempt-only cache.
        let budget = Duration::from_millis(100);
        if !matches!(spec.map(|spec| spec.kind), Some(DbKind::Queue)) {
            return self
                .quick_check_manager
                .quick_check_once(pool, path, budget)
                .await;
        }

        let id = file_id::get_file_id(path)?;
        let checked = self
            .queue_integrity
            .0
            .lock()
            .await
            .entry(id)
            .or_default()
            .clone();
        // Concurrent initializers share the result, not just an attempted check.
        // Cancellation before a result leaves the cell available for a new check.
        let outcome = checked
            .get_or_try_init(|| async {
                let outcome = quick_check(pool, Instant::now() + budget).await?;
                if outcome == QuickCheckOutcome::CorruptedNeedsFixed {
                    telemetry::record_corruption(telemetry_override, Some(DbKind::Queue));
                }
                Ok::<_, anyhow::Error>(outcome)
            })
            .await?;
        if *outcome == QuickCheckOutcome::CorruptedNeedsFixed {
            return Err(anyhow::Error::new(libsqlite3_sys::Error::new(
                libsqlite3_sys::SQLITE_CORRUPT,
            ))
            .context(format!(
                "authoritative queue database at {} is corrupt; preserve it and restore it explicitly before accepting mail or queued input",
                path.display()
            )));
        }
        Ok(QuickCheckOutcome::Skipped)
    }
}
