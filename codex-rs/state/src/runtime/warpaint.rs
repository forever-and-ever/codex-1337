//! TELOS(warpaint): preferences belong to durable thread state, never rollout reconciliation.

use super::StateRuntime;
use crate::sqlite::SqliteWriteQueryExt;
use codex_protocol::ThreadId;

impl StateRuntime {
    pub async fn set_thread_theme(
        &self,
        thread_id: ThreadId,
        theme: Option<&str>,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query("UPDATE threads SET theme = ? WHERE id = ?")
            .bind(theme)
            .bind(thread_id.to_string())
            .execute_with_busy_retry(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
