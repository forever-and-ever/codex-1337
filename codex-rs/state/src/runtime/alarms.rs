use super::StateRuntime;
use codex_install_context::Channel;
use codex_protocol::ThreadId;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadAlarm {
    pub owner: Channel,
    pub id: i64,
    pub thread_id: ThreadId,
    pub note: String,
    pub armed_at: i64,
    pub state: ThreadAlarmState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadAlarmState {
    Armed(AlarmTrigger),
    Sounded { body: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlarmTrigger {
    Clock {
        at: i64,
    },
    Watch {
        session_id: i32,
        command: String,
        pattern: Option<String>,
        deadline: Option<i64>,
    },
}

fn owner_label(owner: Channel) -> &'static str {
    match owner {
        Channel::Production => "production",
        Channel::Staging => "staging",
    }
}

fn alarm_from_row(row: &SqliteRow) -> anyhow::Result<ThreadAlarm> {
    let state = if let Some(body) = row.try_get::<Option<String>, _>("rendered_body")? {
        ThreadAlarmState::Sounded { body }
    } else if let Some(at) = row.try_get::<Option<i64>, _>("clock_at")? {
        ThreadAlarmState::Armed(AlarmTrigger::Clock { at })
    } else {
        ThreadAlarmState::Armed(AlarmTrigger::Watch {
            session_id: row.try_get("session_id")?,
            command: row.try_get("command")?,
            pattern: row.try_get("pattern")?,
            deadline: row.try_get("deadline")?,
        })
    };
    let owner = match row.try_get::<String, _>("owner")?.as_str() {
        "production" => Channel::Production,
        "staging" => Channel::Staging,
        label => anyhow::bail!("invalid alarm owner: {label}"),
    };
    Ok(ThreadAlarm {
        owner,
        id: row.try_get("id")?,
        thread_id: ThreadId::from_string(&row.try_get::<String, _>("thread_id")?)?,
        note: row.try_get("note")?,
        armed_at: row.try_get("armed_at")?,
        state,
    })
}

impl StateRuntime {
    pub async fn list_thread_alarms(
        &self,
        owner: Channel,
        thread_id: ThreadId,
    ) -> anyhow::Result<Vec<ThreadAlarm>> {
        sqlx::query("SELECT * FROM thread_alarms WHERE owner = ? AND thread_id = ? ORDER BY id")
            .bind(owner_label(owner))
            .bind(thread_id.to_string())
            .fetch_all(self.pool.as_ref())
            .await?
            .iter()
            .map(alarm_from_row)
            .collect()
    }

    pub async fn all_thread_alarms(&self, owner: Channel) -> anyhow::Result<Vec<ThreadAlarm>> {
        sqlx::query("SELECT * FROM thread_alarms WHERE owner = ? ORDER BY id")
            .bind(owner_label(owner))
            .fetch_all(self.pool.as_ref())
            .await?
            .iter()
            .map(alarm_from_row)
            .collect()
    }

    pub async fn arm_thread_alarm(
        &self,
        owner: Channel,
        thread_id: ThreadId,
        note: &str,
        armed_at: i64,
        trigger: AlarmTrigger,
    ) -> anyhow::Result<ThreadAlarm> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let thread_id_string = thread_id.to_string();
        let archived = sqlx::query_scalar::<_, bool>("SELECT archived FROM threads WHERE id = ?")
            .bind(&thread_id_string)
            .fetch_optional(&mut *tx)
            .await?;
        anyhow::ensure!(archived.is_some(), "thread not found: {thread_id}");
        anyhow::ensure!(archived == Some(false), "thread is archived: {thread_id}");
        let armed = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM thread_alarms WHERE owner = ? AND thread_id = ? AND rendered_body IS NULL",
        )
        .bind(owner_label(owner))
        .bind(&thread_id_string)
        .fetch_one(&mut *tx)
        .await?;
        anyhow::ensure!(armed < 8, "thread already has eight armed alarms");

        // Optional columns are the SQLite boundary projection of the trigger sum.
        let (at, session_id, command, pattern, deadline) = match &trigger {
            AlarmTrigger::Clock { at } => (Some(*at), None, None, None, None),
            AlarmTrigger::Watch {
                session_id,
                command,
                pattern,
                deadline,
            } => (
                None,
                Some(*session_id),
                Some(command.as_str()),
                pattern.as_deref(),
                *deadline,
            ),
        };
        let id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO thread_alarms (owner, thread_id, note, armed_at, clock_at, session_id, command, pattern, deadline) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(owner_label(owner))
        .bind(&thread_id_string)
        .bind(note)
        .bind(armed_at)
        .bind(at)
        .bind(session_id)
        .bind(command)
        .bind(pattern)
        .bind(deadline)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ThreadAlarm {
            owner,
            id,
            thread_id,
            note: note.to_string(),
            armed_at,
            state: ThreadAlarmState::Armed(trigger),
        })
    }

    pub async fn sound_thread_alarm(
        &self,
        owner: Channel,
        thread_id: ThreadId,
        id: i64,
        body: &str,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "UPDATE thread_alarms SET rendered_body = ?, clock_at = NULL, session_id = NULL, command = NULL, pattern = NULL, deadline = NULL WHERE owner = ? AND thread_id = ? AND id = ? AND rendered_body IS NULL",
        )
        .bind(body)
        .bind(owner_label(owner))
        .bind(thread_id.to_string())
        .bind(id)
        .execute(self.pool.as_ref())
        .await?
        .rows_affected() == 1)
    }

    pub async fn disarm_thread_alarm(
        &self,
        owner: Channel,
        thread_id: ThreadId,
        id: i64,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "DELETE FROM thread_alarms WHERE owner = ? AND thread_id = ? AND id = ? AND rendered_body IS NULL",
        )
        .bind(owner_label(owner))
        .bind(thread_id.to_string())
        .bind(id)
        .execute(self.pool.as_ref())
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn delete_thread_alarm(
        &self,
        owner: Channel,
        thread_id: ThreadId,
        id: i64,
    ) -> anyhow::Result<()> {
        let _ =
            sqlx::query("DELETE FROM thread_alarms WHERE owner = ? AND thread_id = ? AND id = ?")
                .bind(owner_label(owner))
                .bind(thread_id.to_string())
                .bind(id)
                .execute(self.pool.as_ref())
                .await?;
        Ok(())
    }

    pub async fn delete_thread_alarms(
        &self,
        owner: Channel,
        thread_id: ThreadId,
    ) -> anyhow::Result<()> {
        let _ = sqlx::query("DELETE FROM thread_alarms WHERE owner = ? AND thread_id = ?")
            .bind(owner_label(owner))
            .bind(thread_id.to_string())
            .execute(self.pool.as_ref())
            .await?;
        Ok(())
    }
}
