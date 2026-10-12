//! One owned registry placeholder spans durable path admission and child construction.

use super::LocalAgentControl;
use super::spawn::SpawnInitialInput;
use crate::agent::api::AgentInput;
use crate::agent::farm::FarmError;
use crate::agent::farm::FarmReceipt;
use crate::agent::farm::FarmSpawnRequest;
use crate::agent::farm::replay;
use crate::context::ContextualUserFragment;
use crate::context::MAX_DELEGATION_RECEIPT_PATH_BYTES;
use crate::context::UserDelegationReceipt;
use crate::session::farm_allocation::FarmAllocationPhase;
use crate::session::startup::SessionStartup;
use crate::session::startup::SessionStartupGuard;
use codex_protocol::AgentPath;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_state::FarmRejection;
use codex_state::FarmRequestClaim;
use codex_state::FarmRequestState;
use std::sync::Arc;

pub(super) enum SpawnAdmission {
    Ordinary,
    Farm {
        reservation: crate::agent::registry::SpawnReservation,
        startup: Arc<SessionStartup>,
    },
}

impl LocalAgentControl {
    pub(super) async fn check_farm_path_owner(
        &self,
        source: Option<&SessionSource>,
    ) -> CodexResult<()> {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            agent_path: Some(path),
            ..
        })) = source
        else {
            return Ok(());
        };
        let parent = self
            .runtime
            .upgrade()?
            .get_thread(*parent_thread_id)
            .await?;
        if let Some(db) = parent.state_db()
            && db
                .farm_request_id_for_path(*parent_thread_id, path.as_str())
                .await
                .map_err(|error| {
                    CodexErr::Fatal(format!("could not read farm path ownership: {error}"))
                })?
                .is_some()
        {
            return Err(CodexErr::UnsupportedOperation(format!(
                "agent path `{path}` is owned by a historical user delegation"
            )));
        }
        Ok(())
    }

    pub(super) async fn spawn_farm(
        &self,
        request: FarmSpawnRequest,
    ) -> Result<FarmReceipt, FarmError> {
        let FarmSpawnRequest {
            mut spawn,
            payload_sha256,
            task_name,
            authorization,
            task_preview,
            turn,
        } = request;
        let parent_id = spawn.caller;
        let user_request = spawn.options.user_request.as_ref().ok_or_else(|| {
            CodexErr::InvalidRequest("farm requires its captured user boundary".into())
        })?;
        let request_id = user_request.request_id.clone();
        // A captured identity can already belong to a concurrent submission.
        // Losing access to its parent is not evidence that it was never allocated.
        let parent = async { self.runtime.upgrade()?.get_thread(parent_id).await }
            .await
            .map_err(|error| FarmError::Uncertain {
                request_id: request_id.clone(),
                child: None,
                message: format!(
                    "could not inspect the farm parent: {error}; replay this request ID"
                ),
            })?;
        let db = parent.state_db().ok_or_else(|| FarmError::Uncertain {
            request_id: request_id.clone(),
            child: None,
            message: "could not inspect durable farm state; replay this request ID".into(),
        })?;
        if let Some(record) = db
            .farm_request(parent_id, &request_id)
            .await
            .map_err(|error| FarmError::Uncertain {
                request_id: request_id.clone(),
                child: None,
                message: format!(
                    "could not read the existing farm outcome: {error}; replay this request ID"
                ),
            })?
        {
            return replay(&request_id, &payload_sha256, record);
        }
        let admitted = async {
            if spawn.config.ephemeral || parent.session.live_thread().is_none() {
                return Err(CodexErr::InvalidRequest(
                    "farm requires persistent parent and child conversations".into(),
                )
                .into());
            }
            let parent_path = turn
                .session_source
                .get_agent_path()
                .unwrap_or_else(AgentPath::root);
            if let Some(name) = &task_name {
                parent_path.join(name).map_err(CodexErr::InvalidRequest)?;
            }
            let requested_path = spawn
                .source
                .get_agent_path()
                .ok_or_else(|| CodexErr::InvalidRequest("farm requires a child path".into()))?;
            let stem = task_name.as_deref().unwrap_or_else(|| {
                requested_path
                    .as_str()
                    .rsplit('/')
                    .next()
                    .expect("child path has a name")
            });
            let mut reserved = None;
            for suffix in 0..10_000 {
                let tail = if suffix == 0 {
                    String::new()
                } else {
                    format!("_{suffix}")
                };
                let capacity = MAX_DELEGATION_RECEIPT_PATH_BYTES
                    .saturating_sub(parent_path.as_str().len() + 1 + tail.len());
                if capacity == 0 {
                    break;
                }
                let path = parent_path
                    .join(&format!("{}{tail}", &stem[..stem.len().min(capacity)]))
                    .map_err(CodexErr::InvalidRequest)?;
                if path.as_str().len() > MAX_DELEGATION_RECEIPT_PATH_BYTES {
                    return Err(CodexErr::InvalidRequest(
                        "farm child path exceeds receipt limit".into(),
                    )
                    .into());
                }
                let mut reservation = self.runtime.registry.reserve_spawn_slot(None)?;
                if let Err(error) = reservation.reserve_agent_path(&path) {
                    if matches!(error.details(), CodexErrorDetails::UnsupportedOperation(_)) {
                        continue;
                    }
                    return Err(error.into());
                }
                let claim = db
                    .claim_farm_request(parent_id, &request_id, &payload_sha256, path.as_str())
                    .await
                    .map_err(|error| FarmError::Uncertain {
                        request_id: request_id.clone(),
                        child: None,
                        message: format!(
                            "could not confirm farm reservation: {error}; replay this request ID, never submit it as new work"
                        ),
                    })?;
                match claim {
                    FarmRequestClaim::Existing(record) => {
                        return replay(&request_id, &payload_sha256, record);
                    }
                    FarmRequestClaim::PathOccupied => continue,
                    FarmRequestClaim::Claimed => {
                        let SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_path, .. }) =
                            &mut spawn.source
                        else {
                            unreachable!("farm source was validated before reservation");
                        };
                        *agent_path = Some(path);
                        reserved = Some(reservation);
                        break;
                    }
                }
            }
            let reservation = match reserved {
                Some(reservation) => reservation,
                None => {
                    return Err(CodexErr::InvalidRequest("No available farm child path".into()).into());
                }
            };
            let startup = Arc::new(SessionStartup::default());
            let startup_guard = SessionStartupGuard::new(Arc::clone(&startup));
            let result = async {
                parent
                    .session
                    .record_user_delegation(authorization)
                    .await
                    .map_err(|error| {
                        CodexErr::Fatal(format!("could not persist user authorization: {error}"))
                    })?;
                let AgentInput::UserInput(input) = spawn.input else {
                    return Err(CodexErr::InvalidRequest("farm requires user input".into()));
                };
                let (child, _) = self
                    .spawn_agent_reserved(
                        spawn.config,
                        SpawnInitialInput::UserInput(input),
                        Some(spawn.source),
                        spawn.options,
                        SpawnAdmission::Farm {
                            reservation,
                            startup: Arc::clone(&startup),
                        },
                    )
                    .await?;
                // Successful construction has transferred lifetime to the child runtime.
                startup.persistence.lock().await.commit();
                startup_guard.disarm();
                let receipt = FarmReceipt {
                    child_thread_id: child.thread_id,
                    agent_path: child
                        .metadata
                        .agent_path
                        .expect("farm child has its reserved path")
                        .to_string(),
                };
                parent
                    .session
                    .record_conversation_items(
                        &turn,
                        turn.model_info(),
                        &[ContextualUserFragment::into(UserDelegationReceipt {
                            task_preview,
                            agent_path: receipt.agent_path.clone(),
                        })],
                    )
                    .await;
                parent.flush_rollout().await.map_err(|error| {
                    CodexErr::Fatal(format!("could not persist delegation receipt: {error}"))
                })?;
                db.accept_farm_request(parent_id, &request_id)
                    .await
                    .map_err(|error| {
                        CodexErr::Fatal(format!("could not accept farm receipt: {error}"))
                    })?;
                Ok::<_, CodexErr>(receipt)
            }
            .await;
            match result {
                Ok(receipt) => Ok(receipt),
                Err(error) => {
                    let phase = startup.allocation.phase();
                    if matches!(phase, FarmAllocationPhase::BeforeCreation) {
                        let rejection = FarmRejection::from(&error);
                        if db
                            .reject_farm_request(parent_id, &request_id, &rejection)
                            .await
                            .is_ok()
                        {
                            return Err(FarmError::Rejected(rejection.into()));
                        }
                    }
                    let child =
                        db.farm_request(parent_id, &request_id)
                            .await
                            .ok()
                            .flatten()
                            .and_then(|record| match record.state {
                                FarmRequestState::Allocated(child)
                                | FarmRequestState::Accepted(child) => Some(child),
                                _ => None,
                            });
                    let allocation = match phase {
                        FarmAllocationPhase::BeforeCreation => {
                            "rejection persistence was not confirmed".to_owned()
                        }
                        FarmAllocationPhase::CreationMayHaveAllocated(id) => {
                            format!("creation may have allocated {id}")
                        }
                        FarmAllocationPhase::Allocated(id) => format!("child {id} was allocated"),
                    };
                    Err(FarmError::Uncertain {
                        request_id: request_id.clone(),
                        child,
                        message: format!(
                            "Delegation outcome is uncertain: {error}; {allocation}. This submission will not be executed again."
                        ),
                    })
                }
            }
        }
        .await;
        match admitted {
            Err(FarmError::Rejected(error)) => {
                crate::agent::farm::reject_unclaimed(
                    &db,
                    parent_id,
                    &request_id,
                    &payload_sha256,
                    error,
                )
                .await
            }
            outcome => outcome,
        }
    }
}
