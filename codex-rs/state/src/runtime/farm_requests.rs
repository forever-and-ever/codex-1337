//! Direct-user spawn receipts, not a job queue. A claimed request is never executed twice.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FarmRequestRecord {
    pub payload_sha256: String,
    pub child_thread_id: Option<ThreadId>,
    pub agent_path: Option<String>,
    pub accepted: bool,
}

pub enum FarmRequestClaim {
    Claimed,
    Existing(FarmRequestRecord),
}

fn record_from_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<FarmRequestRecord> {
    Ok(FarmRequestRecord {
        payload_sha256: row.try_get("payload_sha256")?,
        child_thread_id: row
            .try_get::<Option<String>, _>("child_thread_id")?
            .map(|id| ThreadId::from_string(&id))
            .transpose()?,
        agent_path: row.try_get("agent_path")?,
        accepted: row.try_get("accepted")?,
    })
}

impl StateRuntime {
    /// Matches only an actual persisted farm branch, never a task-name resemblance.
    pub async fn farm_request_id_for_path(
        &self,
        parent: ThreadId,
        worker: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT request_id FROM thread_farm_requests WHERE parent_thread_id = ? AND (agent_path = ? OR substr(?, 1, length(agent_path) + 1) = agent_path || '/') ORDER BY length(agent_path) DESC LIMIT 1",
        )
        .bind(parent.to_string())
        .bind(worker)
        .bind(worker)
        .fetch_optional(self.pool.as_ref())
        .await?)
    }

    pub async fn farm_request(
        &self,
        parent: ThreadId,
        request_id: &str,
    ) -> anyhow::Result<Option<FarmRequestRecord>> {
        sqlx::query("SELECT payload_sha256, child_thread_id, agent_path, accepted FROM thread_farm_requests WHERE parent_thread_id = ? AND request_id = ?")
            .bind(parent.to_string())
            .bind(request_id)
            .fetch_optional(self.pool.as_ref())
            .await?
            .as_ref()
            .map(record_from_row)
            .transpose()
    }

    /// Atomically claims an identity; even an interrupted claim continues to suppress replay.
    pub async fn claim_farm_request(
        &self,
        parent: ThreadId,
        request_id: &str,
        payload_sha256: &str,
    ) -> anyhow::Result<FarmRequestClaim> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let inserted = sqlx::query_scalar::<_, i64>(
            "INSERT INTO thread_farm_requests (parent_thread_id, request_id, payload_sha256) VALUES (?, ?, ?) ON CONFLICT DO NOTHING RETURNING 1",
        )
        .bind(parent.to_string())
        .bind(request_id)
        .bind(payload_sha256)
        .fetch_optional(&mut *tx)
        .await?;
        let claim = if inserted.is_some() {
            FarmRequestClaim::Claimed
        } else {
            let row = sqlx::query(
                "SELECT payload_sha256, child_thread_id, agent_path, accepted FROM thread_farm_requests WHERE parent_thread_id = ? AND request_id = ?",
            )
            .bind(parent.to_string())
            .bind(request_id)
            .fetch_one(&mut *tx)
            .await?;
            FarmRequestClaim::Existing(record_from_row(&row)?)
        };
        tx.commit().await?;
        Ok(claim)
    }

    /// Records allocation before execution starts, making interrupted outcomes inspectable.
    pub async fn record_farm_child(
        &self,
        parent: ThreadId,
        request_id: &str,
        child: ThreadId,
        agent_path: &str,
        instructions: &str,
    ) -> anyhow::Result<()> {
        let result = sqlx::query(
            "UPDATE thread_farm_requests SET child_thread_id = ?, agent_path = ?, instructions = ? WHERE parent_thread_id = ? AND request_id = ? AND child_thread_id IS NULL",
        )
        .bind(child.to_string())
        .bind(agent_path)
        .bind(instructions)
        .bind(parent.to_string())
        .bind(request_id)
        .execute(self.pool.as_ref())
        .await?;
        anyhow::ensure!(
            result.rows_affected() == 1,
            "farm request allocation was not recorded"
        );
        Ok(())
    }

    /// Restores the child-only additive prompt, independently of current farm preferences.
    pub async fn farm_child_instructions(&self, child: ThreadId) -> anyhow::Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, Option<String>>(
            "SELECT instructions FROM thread_farm_requests WHERE child_thread_id = ?",
        )
        .bind(child.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?
        .flatten())
    }

    /// Acceptance never depends on the child's later lifecycle, including retirement.
    pub async fn accept_farm_request(
        &self,
        parent: ThreadId,
        request_id: &str,
    ) -> anyhow::Result<()> {
        let result = sqlx::query(
            "UPDATE thread_farm_requests SET accepted = 1 WHERE parent_thread_id = ? AND request_id = ? AND child_thread_id IS NOT NULL AND agent_path IS NOT NULL",
        )
        .bind(parent.to_string())
        .bind(request_id)
        .execute(self.pool.as_ref())
        .await?;
        anyhow::ensure!(
            result.rows_affected() == 1,
            "farm request acceptance was not recorded"
        );
        Ok(())
    }
}
