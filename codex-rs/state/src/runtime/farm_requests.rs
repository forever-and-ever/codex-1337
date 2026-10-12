//! Direct-user spawn reservations, not a job queue. An identity is never executed twice.

use super::*;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FarmReceipt {
    pub child_thread_id: ThreadId,
    pub agent_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FarmRejectionKind {
    InvalidRequest,
    UnsupportedOperation,
    Fatal,
}

impl FarmRejectionKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::Fatal => "fatal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FarmRejection {
    pub kind: FarmRejectionKind,
    pub message: String,
}

impl From<&CodexErr> for FarmRejection {
    fn from(error: &CodexErr) -> Self {
        let (kind, message) = match error.details() {
            CodexErrorDetails::InvalidRequest(message) => {
                (FarmRejectionKind::InvalidRequest, message.clone())
            }
            CodexErrorDetails::UnsupportedOperation(message) => {
                (FarmRejectionKind::UnsupportedOperation, message.clone())
            }
            CodexErrorDetails::Fatal(message) => (FarmRejectionKind::Fatal, message.clone()),
            _ => (FarmRejectionKind::Fatal, error.to_string()),
        };
        Self { kind, message }
    }
}

impl From<FarmRejection> for CodexErr {
    fn from(rejection: FarmRejection) -> Self {
        match rejection.kind {
            FarmRejectionKind::InvalidRequest => Self::InvalidRequest(rejection.message),
            FarmRejectionKind::UnsupportedOperation => {
                Self::UnsupportedOperation(rejection.message)
            }
            FarmRejectionKind::Fatal => Self::Fatal(rejection.message),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FarmRequestState {
    /// Old pathless claims cannot prove that no child was allocated.
    LegacyUnknown,
    Reserved {
        agent_path: String,
    },
    Allocated(FarmReceipt),
    Accepted(FarmReceipt),
    Rejected(FarmRejection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FarmRequestRecord {
    pub payload_sha256: String,
    pub state: FarmRequestState,
}

pub enum FarmRequestClaim {
    Claimed,
    Existing(FarmRequestRecord),
    PathOccupied,
}

fn record_from_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<FarmRequestRecord> {
    let path = || -> anyhow::Result<String> { Ok(row.try_get("agent_path")?) };
    let child = || -> anyhow::Result<FarmReceipt> {
        Ok(FarmReceipt {
            child_thread_id: ThreadId::from_string(&row.try_get::<String, _>("child_thread_id")?)?,
            agent_path: path()?,
        })
    };
    let state = match row.try_get::<String, _>("state")?.as_str() {
        "legacy" => FarmRequestState::LegacyUnknown,
        "reserved" => FarmRequestState::Reserved {
            agent_path: path()?,
        },
        "allocated" => FarmRequestState::Allocated(child()?),
        "accepted" => FarmRequestState::Accepted(child()?),
        "rejected" => FarmRequestState::Rejected(FarmRejection {
            kind: match row.try_get::<String, _>("rejection_kind")?.as_str() {
                "invalid_request" => FarmRejectionKind::InvalidRequest,
                "unsupported_operation" => FarmRejectionKind::UnsupportedOperation,
                "fatal" => FarmRejectionKind::Fatal,
                kind => anyhow::bail!("unknown farm rejection kind {kind}"),
            },
            message: row.try_get("rejection_message")?,
        }),
        state => anyhow::bail!("unknown farm request state {state}"),
    };
    Ok(FarmRequestRecord {
        payload_sha256: row.try_get("payload_sha256")?,
        state,
    })
}

impl StateRuntime {
    /// Matches only a persisted farm reservation, never a task-name resemblance.
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
        sqlx::query(
            "SELECT * FROM thread_farm_requests WHERE parent_thread_id = ? AND request_id = ?",
        )
        .bind(parent.to_string())
        .bind(request_id)
        .fetch_optional(self.pool.as_ref())
        .await?
        .as_ref()
        .map(record_from_row)
        .transpose()
    }

    /// The caller holds the registry path reservation through this transaction and spawn.
    pub async fn claim_farm_request(
        &self,
        parent: ThreadId,
        request_id: &str,
        payload_sha256: &str,
        agent_path: &str,
    ) -> anyhow::Result<FarmRequestClaim> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        // Identity takes precedence over path contention, including payload mismatch.
        if let Some(row) = sqlx::query(
            "SELECT * FROM thread_farm_requests WHERE parent_thread_id = ? AND request_id = ?",
        )
        .bind(parent.to_string())
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await?
        {
            let record = record_from_row(&row)?;
            tx.commit().await?;
            return Ok(FarmRequestClaim::Existing(record));
        }
        let inserted = sqlx::query_scalar::<_, i64>(
            "INSERT INTO thread_farm_requests (parent_thread_id, request_id, payload_sha256, agent_path, state) VALUES (?, ?, ?, ?, 'reserved') ON CONFLICT (parent_thread_id, agent_path) DO NOTHING RETURNING 1",
        )
        .bind(parent.to_string())
        .bind(request_id)
        .bind(payload_sha256)
        .bind(agent_path)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(if inserted.is_some() {
            FarmRequestClaim::Claimed
        } else {
            FarmRequestClaim::PathOccupied
        })
    }

    pub async fn reject_unclaimed_farm_request(
        &self,
        parent: ThreadId,
        request_id: &str,
        payload_sha256: &str,
        rejection: &FarmRejection,
    ) -> anyhow::Result<FarmRequestRecord> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO thread_farm_requests (parent_thread_id, request_id, payload_sha256, state, rejection_kind, rejection_message) VALUES (?, ?, ?, 'rejected', ?, ?) ON CONFLICT (parent_thread_id, request_id) DO NOTHING",
        )
        .bind(parent.to_string()).bind(request_id).bind(payload_sha256)
        .bind(rejection.kind.as_str()).bind(&rejection.message)
        .execute(&mut *tx).await?;
        let row = sqlx::query(
            "SELECT * FROM thread_farm_requests WHERE parent_thread_id = ? AND request_id = ?",
        )
        .bind(parent.to_string())
        .bind(request_id)
        .fetch_one(&mut *tx)
        .await?;
        let record = record_from_row(&row)?;
        tx.commit().await?;
        Ok(record)
    }

    /// Only the spawn owner may certify this transition, before entering creation.
    pub async fn reject_farm_request(
        &self,
        parent: ThreadId,
        request_id: &str,
        rejection: &FarmRejection,
    ) -> anyhow::Result<()> {
        let kind = rejection.kind.as_str();
        let result = sqlx::query(
            "UPDATE thread_farm_requests SET state = 'rejected', agent_path = NULL, rejection_kind = ?, rejection_message = ? WHERE parent_thread_id = ? AND request_id = ? AND state = 'reserved'",
        )
        .bind(kind)
        .bind(&rejection.message)
        .bind(parent.to_string())
        .bind(request_id)
        .execute(self.pool.as_ref())
        .await?;
        anyhow::ensure!(
            result.rows_affected() == 1,
            "farm rejection was not recorded"
        );
        Ok(())
    }

    /// Records allocation without changing the path already owned by this request.
    pub async fn record_farm_child(
        &self,
        parent: ThreadId,
        request_id: &str,
        child: ThreadId,
        agent_path: &str,
        instructions: &str,
    ) -> anyhow::Result<()> {
        let result = sqlx::query(
            "UPDATE thread_farm_requests SET child_thread_id = ?, instructions = ?, state = 'allocated' WHERE parent_thread_id = ? AND request_id = ? AND state = 'reserved' AND agent_path = ?",
        )
        .bind(child.to_string())
        .bind(instructions)
        .bind(parent.to_string())
        .bind(request_id)
        .bind(agent_path)
        .execute(self.pool.as_ref())
        .await?;
        anyhow::ensure!(
            result.rows_affected() == 1,
            "farm request allocation was not recorded"
        );
        Ok(())
    }

    pub async fn farm_child_instructions(&self, child: ThreadId) -> anyhow::Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, Option<String>>(
            "SELECT instructions FROM thread_farm_requests WHERE child_thread_id = ?",
        )
        .bind(child.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?
        .flatten())
    }

    /// Acceptance survives the child's later lifecycle, including retirement.
    pub async fn accept_farm_request(
        &self,
        parent: ThreadId,
        request_id: &str,
    ) -> anyhow::Result<()> {
        let result = sqlx::query(
            "UPDATE thread_farm_requests SET state = 'accepted' WHERE parent_thread_id = ? AND request_id = ? AND state = 'allocated'",
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
