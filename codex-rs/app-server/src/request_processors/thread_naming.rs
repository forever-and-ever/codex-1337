//! Name claims are serialized with archive, unarchive, and fork; `SQLite` is the race arbiter.
use super::HashSet;
use super::JSONRPCErrorError;
use super::PersistContext;
use super::Result;
use super::ServerNotification;
use super::StoreReadThreadParams;
use super::StoreThreadMetadataPatch;
use super::ThreadArchiveParams;
use super::ThreadArchivedNotification;
use super::ThreadId;
use super::ThreadNameUpdatedNotification;
use super::ThreadRequestProcessor;
use super::ThreadSetNameParams;
use super::ThreadSetNameResponse;
use super::core_thread_write_error;
use super::internal_error;
use super::invalid_request;
use super::thread_store_mutation_error;
use codex_app_server_protocol::ThreadNameConflict;
use codex_app_server_protocol::ThreadNameConflictError;
use codex_app_server_protocol::ThreadNameMode;
use codex_protocol::protocol::SessionSource;
use codex_state::ThreadMetadata;
use codex_utils_path_uri::LegacyAppPathString;

impl ThreadRequestProcessor {
    pub(super) async fn thread_set_name_response_inner(
        &self,
        params: ThreadSetNameParams,
    ) -> Result<(ThreadSetNameResponse, Option<ThreadNameUpdatedNotification>), JSONRPCErrorError>
    {
        let ThreadSetNameParams {
            thread_id,
            name,
            naming_mode,
            conflict_thread_ids,
        } = params;
        let thread_id = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let name = codex_core::util::normalize_thread_name(&name)
            .ok_or_else(|| invalid_request("thread name must not be empty"))?;
        let mode = naming_mode.unwrap_or(ThreadNameMode::Explicit);
        if mode == ThreadNameMode::Generated && conflict_thread_ids.is_some() {
            return Err(invalid_request(
                "generated names cannot archive conflicting owners",
            ));
        }
        let _permit = self.acquire_thread_list_state_permit().await?;
        self.thread_store
            .ensure_thread_name_claims()
            .await
            .map_err(|err| thread_store_mutation_error("initialize thread names", err))?;
        if let Ok(thread) = self.thread_manager.get_thread(thread_id).await {
            if thread.config_snapshot().await.ephemeral {
                return Err(invalid_request(
                    "ephemeral conversations cannot claim a name",
                ));
            }
            if self
                .state_db
                .as_ref()
                .ok_or_else(|| internal_error("sqlite state db unavailable for thread naming"))?
                .get_thread(thread_id)
                .await
                .map_err(|err| internal_error(format!("failed to read naming target: {err}")))?
                .is_some_and(|thread| thread.archived_at.is_some())
            {
                return Err(invalid_request("archived conversations cannot be renamed"));
            }
            self.thread_store
                .persist_thread(thread_id, PersistContext::Standard)
                .await
                .map_err(|err| thread_store_mutation_error("persist naming target", err))?;
        }
        let claimant = self
            .thread_store
            .read_thread(StoreReadThreadParams {
                thread_id,
                include_archived: false,
                include_history: false,
            })
            .await
            .map_err(|err| thread_store_mutation_error("read naming target", err))?;
        if claimant.archived_at.is_some() {
            return Err(invalid_request("archived conversations cannot be renamed"));
        }
        // Automatic title retries must reuse an already persisted name, not allocate again.
        let current_name = claimant.name;
        if let Some(current_name) = current_name
            && (mode == ThreadNameMode::Generated || current_name == name)
        {
            return Ok((
                ThreadSetNameResponse {
                    thread_name: Some(current_name),
                },
                None,
            ));
        }
        let archived_ids = if mode == ThreadNameMode::Explicit {
            self.release_confirmed_name_owners(
                &name,
                Some(thread_id),
                conflict_thread_ids.as_deref(),
            )
            .await?
        } else {
            Vec::new()
        };
        let name = self
            .claim_thread_name(thread_id, &name, mode)
            .await
            .map_err(|mut err| {
                if !archived_ids.is_empty() {
                    let data = err.data.get_or_insert_with(|| serde_json::json!({}));
                    data["archivedThreadIds"] = serde_json::json!(archived_ids);
                    err.message = format!(
                        "Previous owner was archived, but rename failed: {}",
                        err.message
                    );
                }
                err
            })?;
        Ok((
            ThreadSetNameResponse {
                thread_name: Some(name.clone()),
            },
            Some(ThreadNameUpdatedNotification {
                thread_id: thread_id.to_string(),
                thread_name: Some(name),
            }),
        ))
    }

    async fn name_owners(
        &self,
        name: &str,
        claimant: Option<ThreadId>,
    ) -> Result<Vec<ThreadMetadata>, JSONRPCErrorError> {
        self.thread_store
            .ensure_thread_name_claims()
            .await
            .map_err(|err| thread_store_mutation_error("initialize thread names", err))?;
        if let Some(id) = claimant
            && let Some(metadata) = self
                .state_db
                .as_ref()
                .ok_or_else(|| internal_error("sqlite state db unavailable for thread naming"))?
                .get_thread(id)
                .await
                .map_err(|err| internal_error(format!("failed to read naming target: {err}")))?
            && serde_json::from_str::<SessionSource>(&metadata.source).is_ok_and(|source| {
                matches!(
                    source,
                    SessionSource::SubAgent(_) | SessionSource::Internal(_)
                )
            })
        {
            return Ok(Vec::new());
        }
        let owners = self
            .state_db
            .as_ref()
            .ok_or_else(|| internal_error("sqlite state db unavailable for thread naming"))?
            .find_thread_name_owners(name)
            .await
            .map_err(|err| internal_error(format!("failed to resolve thread name: {err}")))?;
        Ok(owners
            .into_iter()
            .filter(|owner| Some(owner.id) != claimant)
            .collect())
    }

    async fn name_conflict(&self, name: &str, owners: Vec<ThreadMetadata>) -> JSONRPCErrorError {
        let mut conflicts = Vec::with_capacity(owners.len());
        for owner in owners {
            conflicts.push(ThreadNameConflict {
                thread_id: owner.id.to_string(),
                name: owner.name.unwrap_or_else(|| name.to_owned()),
                cwd: LegacyAppPathString::from_path(&owner.cwd),
                status: self
                    .thread_watch_manager
                    .loaded_status_for_thread(&owner.id.to_string())
                    .await,
            });
        }
        let data = ThreadNameConflictError::Conflict {
            name: name.to_owned(),
            conflicts,
            archive_stops_descendants: true,
        };
        let data = match serde_json::to_value(data) {
            Ok(data) => data,
            Err(err) => return internal_error(format!("failed to serialize name conflict: {err}")),
        };
        JSONRPCErrorError {
            code: crate::error_code::INVALID_REQUEST_ERROR_CODE,
            message: format!("Conversation name {name:?} is already owned"),
            data: Some(data),
        }
    }

    /// Consent binds concrete owners, never a name whose ownership may have changed.
    pub(super) async fn confirmed_name_owners(
        &self,
        name: &str,
        claimant: Option<ThreadId>,
        confirmed_ids: Option<&[String]>,
    ) -> Result<Vec<ThreadMetadata>, JSONRPCErrorError> {
        let owners = self.name_owners(name, claimant).await?;
        if owners.is_empty() {
            return Ok(Vec::new());
        }
        let actual_ids = owners
            .iter()
            .map(|owner| owner.id.to_string())
            .collect::<HashSet<_>>();
        let confirmed_ids = confirmed_ids.map(|ids| ids.iter().cloned().collect::<HashSet<_>>());
        if confirmed_ids.as_ref() != Some(&actual_ids) {
            return Err(self.name_conflict(name, owners).await);
        }
        Ok(owners)
    }

    pub(super) async fn release_confirmed_name_owners(
        &self,
        name: &str,
        claimant: Option<ThreadId>,
        confirmed_ids: Option<&[String]>,
    ) -> Result<Vec<String>, JSONRPCErrorError> {
        let owners = self
            .confirmed_name_owners(name, claimant, confirmed_ids)
            .await?;
        // The confirmed normal archive stops the owner and its spawned descendants, including
        // running work. Never allow that family to contain the conversation being renamed.
        for owner in &owners {
            for id in self.state_db_spawn_subtree_thread_ids(owner.id).await? {
                if Some(id) == claimant {
                    return Err(invalid_request(
                        "a conversation cannot archive its own family to claim a name",
                    ));
                }
            }
        }
        let mut archived_ids = Vec::new();
        for owner in owners {
            let (_, ids) = self
                .thread_archive_response(ThreadArchiveParams {
                    thread_id: owner.id.to_string(),
                })
                .await?;
            for id in &ids {
                self.outgoing
                    .send_server_notification(ServerNotification::ThreadArchived(
                        ThreadArchivedNotification {
                            thread_id: id.clone(),
                        },
                    ))
                    .await;
            }
            archived_ids.extend(ids);
        }
        Ok(archived_ids)
    }

    pub(super) async fn claim_thread_name(
        &self,
        thread_id: ThreadId,
        base: &str,
        mode: ThreadNameMode,
    ) -> Result<String, JSONRPCErrorError> {
        // A claimed name is durable even before the first user turn; staged-only names cannot
        // arbitrate concurrent clients or survive reconnects.
        if let Ok(thread) = self.thread_manager.get_thread(thread_id).await {
            if thread.config_snapshot().await.ephemeral {
                return Err(invalid_request(
                    "ephemeral conversations cannot claim a name",
                ));
            }
            self.thread_store
                .persist_thread(thread_id, PersistContext::Standard)
                .await
                .map_err(|err| thread_store_mutation_error("persist named thread", err))?;
        }
        for suffix in 0..=u32::MAX {
            let name = if suffix == 0 {
                base.to_owned()
            } else {
                format!("{base} ({suffix})")
            };
            let owners = self.name_owners(&name, Some(thread_id)).await?;
            if !owners.is_empty() {
                if mode == ThreadNameMode::Generated {
                    continue;
                }
                return Err(self.name_conflict(&name, owners).await);
            }
            match self
                .thread_manager
                .update_thread_metadata(
                    thread_id,
                    StoreThreadMetadataPatch {
                        name: Some(Some(name.clone())),
                        ..Default::default()
                    },
                    /*include_archived*/ false,
                )
                .await
            {
                Ok(_) => return Ok(name),
                Err(err) => {
                    let owners = self.name_owners(&name, Some(thread_id)).await?;
                    if !owners.is_empty() {
                        if mode == ThreadNameMode::Generated {
                            continue;
                        }
                        return Err(self.name_conflict(&name, owners).await);
                    }
                    let mut error = core_thread_write_error("set thread name", err);
                    if let Some(state) = &self.state_db
                        && let Ok(Some(current)) = state.get_thread(thread_id).await
                    {
                        error.data = Some(serde_json::json!({"currentThreadName": current.name}));
                    }
                    return Err(error);
                }
            }
        }
        Err(internal_error(
            "generated conversation name suffixes exhausted",
        ))
    }
}
