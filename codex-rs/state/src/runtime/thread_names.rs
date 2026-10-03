use super::*;

pub(super) fn initial_thread_name(metadata: &ThreadMetadata) -> Option<&str> {
    metadata
        .name
        .as_deref()
        .or_else(|| {
            (metadata.history_mode == codex_protocol::protocol::ThreadHistoryMode::Legacy
                && metadata.first_user_message.as_deref().map(str::trim)
                    != Some(metadata.title.trim()))
            .then_some(metadata.title.as_str())
        })
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
#[path = "thread_names_tests.rs"]
mod tests;

impl StateRuntime {
    pub(super) async fn unarchive_with_name_claim(
        &self,
        thread_id: ThreadId,
        rollout_path: &Path,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        // Acquire SQLite's writer lock before reading candidates. A name assignment racing this
        // restore cannot slip between the availability check and the unarchive commit.
        sqlx::query("UPDATE threads SET name = name WHERE id = ?")
            .bind(thread_id.to_string())
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(
            r#"SELECT name,
    CASE WHEN json_valid(source)
        THEN json_type(source, '$.subagent') IS NULL
            AND json_type(source, '$.internal') IS NULL
        ELSE 1 END AS claims_name
FROM threads WHERE id = ?"#,
        )
        .bind(thread_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(());
        };
        let name: Option<String> = row.try_get("name")?;
        if row.try_get::<bool, _>("claims_name")?
            && let Some(name) = name
        {
            let base = format!(
                "{name}_unarchived_{}",
                Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            );
            let mut candidate = name;
            let mut suffix = 0_u64;
            loop {
                let taken = sqlx::query_scalar::<_, bool>(
                    r#"SELECT EXISTS(SELECT 1 FROM threads
WHERE archived = 0 AND id != ? AND name = ? COLLATE NOCASE
    AND CASE WHEN json_valid(source)
        THEN json_type(source, '$.subagent') IS NULL
            AND json_type(source, '$.internal') IS NULL
        ELSE 1 END)"#,
                )
                .bind(thread_id.to_string())
                .bind(&candidate)
                .fetch_one(&mut *tx)
                .await?;
                if !taken {
                    break;
                }
                candidate = if suffix == 0 {
                    base.clone()
                } else {
                    format!("{base}({suffix})")
                };
                suffix = suffix
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("unarchive name space exhausted"))?;
            }
            // Persist the name before releasing the archived state, in the same transaction.
            sqlx::query("UPDATE threads SET name = ? WHERE id = ?")
                .bind(candidate)
                .bind(thread_id.to_string())
                .execute(&mut *tx)
                .await?;
        }
        let updated_at = file_modified_time_utc(rollout_path).await;
        sqlx::query(
            "UPDATE threads SET archived = 0, archived_at = NULL, rollout_path = ?, updated_at = COALESCE(?, updated_at), updated_at_ms = COALESCE(?, updated_at_ms) WHERE id = ?",
        )
        .bind(rollout_path.display().to_string())
        .bind(updated_at.map(datetime_to_epoch_seconds))
        .bind(updated_at.map(datetime_to_epoch_millis))
        .bind(thread_id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Resolve active canonical names, with trimmed input and SQLite's ASCII NOCASE matching.
    /// Internal sessions and every subagent source are outside the root-name namespace.
    pub async fn find_thread_name_owners(&self, name: &str) -> anyhow::Result<Vec<ThreadMetadata>> {
        anyhow::ensure!(
            self.thread_name_claims_imported().await?,
            "legacy thread names must be imported before resolving name claims"
        );
        let ids = sqlx::query_scalar::<_, String>(
            r#"SELECT id FROM threads
WHERE archived = 0 AND name = ? COLLATE NOCASE
    AND CASE WHEN json_valid(source)
        THEN json_type(source, '$.subagent') IS NULL
            AND json_type(source, '$.internal') IS NULL
        ELSE 1 END
ORDER BY id"#,
        )
        .bind(name.trim())
        .fetch_all(self.pool.as_ref())
        .await?;
        let mut owners = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(metadata) = self.get_thread(ThreadId::from_string(&id)?).await? {
                owners.push(metadata);
            }
        }
        Ok(owners)
    }

    /// Whether the legacy name projection has been imported into canonical claims.
    pub async fn thread_name_claims_imported(&self) -> anyhow::Result<bool> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT singleton FROM thread_name_claims_import")
                .fetch_optional(self.pool.as_ref())
                .await?
                .is_some(),
        )
    }

    /// Import names once without allowing stale projections to overwrite canonical claims.
    /// The transaction and unique index make a conflicting legacy import fail atomically.
    pub async fn import_legacy_thread_names(
        &self,
        names: &[(ThreadId, String)],
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let inserted =
            sqlx::query("INSERT OR IGNORE INTO thread_name_claims_import (singleton) VALUES (1)")
                .execute(&mut *tx)
                .await?;
        if inserted.rows_affected() != 0 {
            for (id, name) in names {
                sqlx::query(
                    "UPDATE threads SET name = ? WHERE id = ? AND history_mode = 'legacy' AND (name IS NULL OR (source = '{\"subagent\":{\"other\":\"guardian\"}}' AND name = 'Guardian review'))",
                )
                .bind(Some(name.trim()).filter(|name| !name.is_empty()))
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(|err| anyhow::anyhow!("legacy thread name import conflicts for {id}: {err}"))?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
}
