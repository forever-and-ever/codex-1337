//! Authoritative accepted ordinary mail; canonical rollout admission retires its payload.
//! Terminal origin identities survive with their originating durable conversation.

use super::SqliteQueueStore;
use crate::MAX_QUEUE_ITEMS;
use anyhow::Result;
use anyhow::ensure;
use codex_protocol::ResponseItemId;
use codex_protocol::ThreadId;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentMailState {
    Pending(String),
    Admitted,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMailRecord {
    pub id: ResponseItemId,
    pub root: ThreadId,
    pub sender: ThreadId,
    pub recipient: ThreadId,
    pub state: AgentMailState,
}

fn from_row(row: &SqliteRow) -> Result<AgentMailRecord> {
    let state: String = row.try_get("state")?;
    let payload: Option<String> = row.try_get("payload_json")?;
    let state = match (state.as_str(), payload) {
        ("pending", Some(payload)) => AgentMailState::Pending(payload),
        ("admitted", None) => AgentMailState::Admitted,
        ("cancelled", None) => AgentMailState::Cancelled,
        _ => anyhow::bail!("invalid authoritative agent-mail record"),
    };
    Ok(AgentMailRecord {
        id: ResponseItemId::from_server(row.try_get("id")?),
        root: ThreadId::try_from(row.try_get::<String, _>("root_thread_id")?)?,
        sender: ThreadId::try_from(row.try_get::<String, _>("sender_thread_id")?)?,
        recipient: ThreadId::try_from(row.try_get::<String, _>("recipient_thread_id")?)?,
        state,
    })
}

impl SqliteQueueStore {
    pub async fn accept_agent_mail(
        &self,
        id: &ResponseItemId,
        root: ThreadId,
        sender: ThreadId,
        recipient: ThreadId,
        origin: &str,
        payload_sha256: &str,
        payload_json: &str,
    ) -> Result<AgentMailRecord> {
        let mut tx = self.pool.begin().await?;
        // The write reserves SQLite's writer before reading the existing identity.
        // AUTOINCREMENT preserves acceptance order even after payload/tombstone cleanup.
        sqlx::query(
            "INSERT INTO agent_mail
             (id, root_thread_id, sender_thread_id, recipient_thread_id, origin,
              payload_sha256, state, payload_json)
             SELECT ?, ?, ?, ?, ?, ?, 'pending', ?
             WHERE (SELECT COUNT(*) FROM agent_mail
                    WHERE recipient_thread_id = ? AND state = 'pending') < ?
             ON CONFLICT(sender_thread_id, recipient_thread_id, origin) DO NOTHING",
        )
        .bind(id.as_str())
        .bind(root.to_string())
        .bind(sender.to_string())
        .bind(recipient.to_string())
        .bind(origin)
        .bind(payload_sha256)
        .bind(payload_json)
        .bind(recipient.to_string())
        .bind(i64::try_from(MAX_QUEUE_ITEMS)?)
        .execute(tx.as_mut())
        .await?;
        let row = sqlx::query(
            "SELECT * FROM agent_mail
             WHERE sender_thread_id = ? AND recipient_thread_id = ? AND origin = ?",
        )
        .bind(sender.to_string())
        .bind(recipient.to_string())
        .bind(origin)
        .fetch_optional(tx.as_mut())
        .await?
        .ok_or_else(|| anyhow::anyhow!("ordinary agent-mail inbox is full"))?;
        ensure!(
            row.try_get::<String, _>("payload_sha256")? == payload_sha256
                && row.try_get::<String, _>("root_thread_id")? == root.to_string(),
            "agent-mail origin was already accepted with different content or authority"
        );
        let record = from_row(&row)?;
        tx.commit().await?;
        Ok(record)
    }

    pub async fn agent_mail(
        &self,
        recipient: ThreadId,
        id: &ResponseItemId,
    ) -> Result<Option<AgentMailRecord>> {
        sqlx::query("SELECT * FROM agent_mail WHERE recipient_thread_id = ? AND id = ?")
            .bind(recipient.to_string())
            .bind(id.as_str())
            .fetch_optional(self.pool.as_ref())
            .await?
            .as_ref()
            .map(from_row)
            .transpose()
    }

    pub async fn pending_agent_mail(&self, recipient: ThreadId) -> Result<Vec<AgentMailRecord>> {
        sqlx::query(
            "SELECT * FROM agent_mail
             WHERE recipient_thread_id = ? AND state = 'pending' ORDER BY sequence",
        )
        .bind(recipient.to_string())
        .fetch_all(self.pool.as_ref())
        .await?
        .iter()
        .map(from_row)
        .collect()
    }

    pub async fn pending_agent_mail_recipients(&self) -> Result<Vec<(ThreadId, ThreadId)>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT root_thread_id, recipient_thread_id FROM agent_mail
             WHERE state = 'pending' GROUP BY root_thread_id, recipient_thread_id
             ORDER BY MIN(sequence)",
        )
        .fetch_all(self.pool.as_ref())
        .await?;
        rows.into_iter()
            .map(|(root, recipient)| {
                Ok((ThreadId::try_from(root)?, ThreadId::try_from(recipient)?))
            })
            .collect()
    }

    pub async fn admit_agent_mail(&self, recipient: ThreadId, id: &ResponseItemId) -> Result<()> {
        let result = sqlx::query(
            "UPDATE agent_mail SET state = 'admitted', payload_json = NULL
             WHERE recipient_thread_id = ? AND id = ? AND state IN ('pending', 'admitted')",
        )
        .bind(recipient.to_string())
        .bind(id.as_str())
        .execute(self.pool.as_ref())
        .await?;
        ensure!(
            result.rows_affected() == 1,
            "accepted agent-mail identity disappeared or was cancelled"
        );
        Ok(())
    }

    pub async fn cancel_agent_mail_recipients(&self, recipients: &[ThreadId]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for recipient in recipients {
            sqlx::query(
                "UPDATE agent_mail SET state = 'cancelled', payload_json = NULL
                 WHERE recipient_thread_id = ? AND state = 'pending'",
            )
            .bind(recipient.to_string())
            .execute(tx.as_mut())
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn delete_thread_agent_mail(&self, thread: ThreadId) -> Result<()> {
        // Explicit conversation deletion cancels its pending obligations and identities.
        // Retirement keeps originating histories, so it only cancels recipient obligations.
        sqlx::query(
            "DELETE FROM agent_mail
             WHERE sender_thread_id = ? OR recipient_thread_id = ? OR root_thread_id = ?",
        )
        .bind(thread.to_string())
        .bind(thread.to_string())
        .bind(thread.to_string())
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }
}
