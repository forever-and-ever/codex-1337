//! Selects persistent root threads for managed daemon recovery using normal resume semantics.
//! Only threads successfully persisted through the existing thread store become candidates.

use super::ThreadRequestProcessor;
use codex_app_server_transport::daemon_recovery::InterruptedTurn;
use codex_app_server_transport::daemon_recovery::RecoverySnapshot;
use codex_thread_store::PersistContext;
use tracing::warn;

impl ThreadRequestProcessor {
    pub(crate) async fn restore_openrouter_candidates(&self, snapshot: &mut RecoverySnapshot) {
        match codex_core::recover_openrouter_batches(&self.config).await {
            Ok(jobs) => {
                for (thread, turn, owner) in jobs {
                    let id = thread.to_string();
                    snapshot.loaded.insert(owner.root.to_string());
                    snapshot.loaded.insert(id.clone());
                    if thread != owner.root {
                        snapshot.paid_children.insert(id.clone());
                    }
                    snapshot
                        .interrupted
                        .entry(id)
                        .or_insert_with(|| InterruptedTurn {
                            turn_id: turn,
                            prompt_cache_ttl: owner.prompt_cache_ttl,
                            output_schema: None,
                            service_tier: None,
                            cyber_access_program: owner.cyber_access_program,
                            local_environment: Some(codex_app_server_protocol::ThreadEnvironment {
                                environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.into(),
                                cwd: owner.cwd.into(),
                                runtime_workspace_roots: owner
                                    .workspace_roots
                                    .into_iter()
                                    .map(Into::into)
                                    .collect(),
                            }),
                        });
                }
            }
            Err(error) => tracing::warn!(%error,"could not discover pending OpenRouter jobs"),
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "snapshot selection must be serialized against pending unloads"
    )]
    pub(crate) async fn daemon_recovery_snapshot(&self) -> RecoverySnapshot {
        let mut snapshot = RecoverySnapshot::default();
        for thread_id in self.thread_manager.list_thread_ids().await {
            let pending_unloads = self.pending_thread_unloads.lock().await;
            if pending_unloads.contains_key(&thread_id) {
                continue;
            }
            let Ok(thread) = self.thread_manager.get_thread(thread_id).await else {
                continue;
            };
            let config = thread.config_snapshot().await;
            let interrupted = thread.interrupted_turn().await;
            let paid = if let Some((turn, ..)) = &interrupted {
                thread.has_paid_openrouter_step(turn).await.unwrap_or(false)
            } else {
                false
            };
            if !config.ephemeral
                && (paid
                    || config.parent_thread_id.is_none()
                        && !config.session_source.is_non_root_agent())
            {
                if let Err(err) = self
                    .thread_store
                    .persist_thread(thread_id, PersistContext::Standard)
                    .await
                {
                    warn!(%thread_id, %err, "skipping daemon restore for thread that could not be persisted");
                    continue;
                }
                snapshot.loaded.insert(thread_id.to_string());
                if paid && config.parent_thread_id.is_some() {
                    snapshot.paid_children.insert(thread_id.to_string());
                }
                if let Some((turn_id, options, environment)) = interrupted {
                    snapshot.interrupted.insert(
                        thread_id.to_string(),
                        InterruptedTurn {
                            prompt_cache_ttl: options.prompt_cache_ttl,
                            turn_id,
                            output_schema: options.final_output_json_schema,
                            service_tier: options.service_tier,
                            cyber_access_program: options.cyber_access_program,
                            local_environment: Some((&environment).into()),
                        },
                    );
                }
            }
        }
        snapshot
    }

    pub(crate) async fn park_openrouter_steps(&self, snapshot: &RecoverySnapshot) {
        for (id, saved) in &snapshot.interrupted {
            let Ok(thread_id) = codex_protocol::ThreadId::from_string(id) else {
                continue;
            };
            let Ok(thread) = self.thread_manager.get_thread(thread_id).await else {
                continue;
            };
            match thread.park_openrouter_step(&saved.turn_id).await {
                Ok(true) => self.thread_watch_manager.note_thread_shutdown(id).await,
                Ok(false) => {}
                Err(error) => tracing::warn!(%thread_id,%error,"failed to park paid step"),
            }
        }
    }
}
