use std::collections::HashMap;

use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use tokio::io::AsyncBufReadExt;

use super::LocalThreadStore;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

pub(super) async fn ensure_thread_name_claims(store: &LocalThreadStore) -> ThreadStoreResult<()> {
    store
        .thread_name_claims
        .get_or_try_init(|| async {
            let Some(db) = store.state_db().await else {
                // SQLite-less legacy readers remain supported, but cannot assign names.
                return Ok(());
            };
            let imported = db
                .thread_name_claims_imported()
                .await
                .map_err(name_claim_error)?;
            let names = if !imported {
                read_legacy_names(store, db.as_ref()).await?
            } else {
                Vec::new()
            };
            db.import_legacy_thread_names(&names)
                .await
                .map_err(name_claim_error)
        })
        .await?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct LegacyNameEntry {
    id: ThreadId,
    thread_name: String,
}

async fn read_legacy_names(
    store: &LocalThreadStore,
    db: &codex_state::StateRuntime,
) -> ThreadStoreResult<Vec<(ThreadId, String)>> {
    let path = store.config.codex_home.join("session_index.jsonl");
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(name_claim_error(err)),
    };
    let mut lines = tokio::io::BufReader::new(file).lines();
    let mut names = HashMap::new();
    while let Some(line) = lines.next_line().await.map_err(name_claim_error)? {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<LegacyNameEntry>(&line) else {
            tracing::warn!("skipping malformed legacy thread name-index entry");
            continue;
        };
        names.insert(entry.id, entry.thread_name.trim().to_string());
    }
    for (&id, name) in &names {
        if name.is_empty() || db.get_thread(id).await.map_err(name_claim_error)?.is_some() {
            continue;
        }
        let path =
            codex_rollout::find_rollout_path_by_rollout_id(store.config.codex_home.as_path(), id)
                .await
                .map_err(name_claim_error)?;
        if let Some(path) = path {
            let archived =
                super::helpers::rollout_path_is_archived(store.config.codex_home.as_path(), &path);
            let meta = codex_rollout::read_session_meta_line(&path)
                .await
                .map_err(name_claim_error)?;
            if !matches!(
                meta.meta.source,
                SessionSource::SubAgent(_) | SessionSource::Internal(_)
            ) {
                codex_rollout::state_db::reconcile_rollout(
                    Some(db),
                    &path,
                    &store.config.default_model_provider_id,
                    /*builder*/ None,
                    &[],
                    /*archived_only*/ Some(archived),
                    /*new_thread_memory_mode*/ None,
                )
                .await;
                if db.get_thread(id).await.map_err(name_claim_error)?.is_none() {
                    return Err(name_claim_error(format!(
                        "named legacy thread {id} has no canonical SQLite row after reconciliation"
                    )));
                }
            }
        }
    }
    Ok(names.into_iter().collect())
}

fn name_claim_error(err: impl std::fmt::Display) -> ThreadStoreError {
    ThreadStoreError::Conflict {
        message: format!("cannot establish canonical thread name claims: {err}"),
    }
}
