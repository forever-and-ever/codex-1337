//! Direct user delegation through the ordinary shared child-spawn path.

use super::api::AgentInput;
use super::api::SpawnRequest;
use super::child_config::SpawnConfigOptions;
use super::child_config::SpawnConfigVersion;
use super::child_config::SpawnModelPrecedence;
use super::child_config::prepare_agent_spawn_config_from_settings;
use super::control::spawn::load_agent_model_context;
use super::exceeds_thread_spawn_depth_limit;
use super::next_thread_spawn_depth;
use super::types::SpawnAgentForkMode;
use super::types::SpawnAgentOptions;
use super::types::UserSpawnRequest;
use crate::codex_thread::CodexThread;
use crate::context::ContextualUserFragment;
use crate::context::MAX_DELEGATION_RECEIPT_PATH_BYTES;
use crate::context::UserDelegationReceipt;
use crate::session::thread_settings;
use codex_history::RetainedUserMessage;
use codex_history::UserInputOrigin;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use codex_state::FarmRequestClaim;
use codex_state::FarmRequestRecord;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::sync::Arc;
use uuid::Uuid;

const CHILD_INSTRUCTIONS: &str = "The user delegated the following task to you. Work within that scope; inherited conversation, if any, is context, not additional work. Coordinate consequential assumptions with your parent and report results, validation, and remaining risks. The parent retains review and integration responsibility.";
const MAX_EXTRA_INSTRUCTIONS_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FarmMode {
    FullHistory,
    Fresh,
}

pub struct FarmRequest {
    pub request_id: String,
    pub mode: FarmMode,
    pub input: Vec<UserInput>,
    pub task_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FarmReceipt {
    pub child_thread_id: ThreadId,
    pub agent_path: String,
}

/// Only a definitive rejection may safely be edited and submitted as a different request.
#[derive(Debug)]
pub enum FarmError {
    Rejected(CodexErr),
    IdentityMismatch {
        request_id: String,
        child: Option<FarmReceipt>,
    },
    Uncertain {
        request_id: String,
        child: Option<FarmReceipt>,
        message: String,
    },
}

pub(crate) fn apply_farm_instructions(config: &mut crate::config::Config, instructions: &str) {
    if config
        .developer_instructions
        .as_deref()
        .is_none_or(|existing| !existing.ends_with(instructions))
    {
        config.developer_instructions = Some(match config.developer_instructions.take() {
            Some(existing) if !existing.is_empty() => format!("{existing}\n\n{instructions}"),
            Some(_) | None => instructions.to_owned(),
        });
    }
    config.multi_agent_v2.subagent_developer_instructions = config.developer_instructions.clone();
}

impl From<CodexErr> for FarmError {
    fn from(error: CodexErr) -> Self {
        Self::Rejected(error)
    }
}

fn replay(
    request_id: &str,
    payload_sha256: &str,
    record: FarmRequestRecord,
) -> Result<FarmReceipt, FarmError> {
    let child =
        record
            .child_thread_id
            .zip(record.agent_path)
            .map(|(child_thread_id, agent_path)| FarmReceipt {
                child_thread_id,
                agent_path,
            });
    if record.payload_sha256 != payload_sha256 {
        return Err(FarmError::IdentityMismatch {
            request_id: request_id.to_owned(),
            child,
        });
    }
    if record.accepted
        && let Some(child) = child.as_ref()
    {
        return Ok(child.clone());
    }
    Err(FarmError::Uncertain {
        request_id: request_id.to_owned(),
        child,
        message: "Delegation outcome is uncertain. The child may already be running; inspect the agent roster before taking further action. This submission will not be executed again.".to_owned(),
    })
}

impl CodexThread {
    /// Accepts genuine user input without starting, steering, or interrupting this parent.
    /// Call only from a user-facing host action, never from an agent's tool request.
    pub async fn farm(&self, request: FarmRequest) -> Result<FarmReceipt, FarmError> {
        let request_uuid = Uuid::parse_str(&request.request_id)
            .map_err(|_| CodexErr::InvalidRequest("farm requestId must be a UUID".to_owned()))?;
        let request_id = request_uuid.to_string();
        // Keep ordinary farm hashes compatible with receipts created before named farms.
        let payload = match &request.task_name {
            Some(name) => serde_json::to_vec(&(request.mode, &request.input, name)),
            None => serde_json::to_vec(&(request.mode, &request.input)),
        }
        .map_err(|error| {
            CodexErr::InvalidRequest(format!("could not encode farm submission: {error}"))
        })?;
        let payload_sha256 = format!("{:x}", Sha256::digest(payload));
        let db = self.state_db().ok_or_else(|| {
            CodexErr::InvalidRequest("farm requires durable thread state".to_owned())
        })?;
        if let Some(record) = db
            .farm_request(self.session.thread_id, &request_id)
            .await
            .map_err(|error| CodexErr::Fatal(format!("could not read farm receipt: {error}")))?
        {
            return replay(&request_id, &payload_sha256, record);
        }
        let task = request
            .input
            .iter()
            .filter_map(|item| match item {
                UserInput::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if task.trim().is_empty() {
            return Err(
                CodexErr::InvalidRequest("farm requires a nonempty task".to_owned()).into(),
            );
        }
        let command = match request.mode {
            FarmMode::FullHistory => "/farm",
            FarmMode::Fresh => "/farm_fresh",
        };
        let authorization = RetainedUserMessage {
            turn_id: String::new(),
            message_id: Some(format!("farm:{request_id}")),
            text: format!("{command} {task}"),
            complete: request
                .input
                .iter()
                .all(|item| matches!(item, UserInput::Text { .. })),
            origin: UserInputOrigin::User,
            phase: None,
        };
        if !authorization.fits_storage_bound() {
            return Err(CodexErr::InvalidRequest(
                "farm task exceeds the retained authorization limit".to_owned(),
            )
            .into());
        }

        // Materialization checkpoints the initial account binding under the same
        // nonreentrant permit used below. It must finish before the snapshot boundary.
        self.ensure_rollout_materialized().await;
        // Settings, ready environment bindings, and the ordinary stored fork input are
        // captured under the checkpoint/settings boundary, never a whole-turn/tool lock.
        let (turn, fork_history) = {
            let _boundary = thread_settings::acquire_persistence_lock(&self.session).await;
            let turn = self.session.new_inject_items_context().await;
            self.flush_rollout()
                .await
                .map_err(|error| CodexErr::Fatal(format!("could not flush parent: {error}")))?;
            let history = if request.mode == FarmMode::FullHistory {
                let state = self.session.services.local_agent_runtime.upgrade()?;
                Some(Arc::new(
                    load_agent_model_context(&state, self.session.thread_id, turn.history_mode)
                        .await?
                        .ok_or_else(|| {
                            CodexErr::InvalidRequest(
                                "parent history unavailable for farm".to_owned(),
                            )
                        })?,
                ))
            } else {
                None
            };
            (turn, history)
        };
        if turn.multi_agent_version != MultiAgentVersion::V2 {
            return Err(CodexErr::InvalidRequest("farm requires multi-agent v2".to_owned()).into());
        }
        if turn.config.ephemeral || self.session.live_thread().is_none() {
            return Err(CodexErr::InvalidRequest(
                "farm requires a persistent parent conversation".to_owned(),
            )
            .into());
        }
        let fork_mode =
            (request.mode == FarmMode::FullHistory).then_some(SpawnAgentForkMode::FullHistory);
        let farm_config = &turn.config.wrangler.farm;
        let extra = farm_config.instructions.as_deref().unwrap_or_default();
        if extra.len() > MAX_EXTRA_INSTRUCTIONS_BYTES {
            return Err(CodexErr::InvalidRequest(
                "wrangler.farm.instructions exceeds 512 bytes".to_owned(),
            )
            .into());
        }
        let instructions = if extra.is_empty() {
            CHILD_INSTRUCTIONS.to_owned()
        } else {
            format!("{CHILD_INSTRUCTIONS}\n\n{extra}")
        };
        let prepared = prepare_agent_spawn_config_from_settings(
            &self.session,
            &turn,
            &turn.initial_settings,
            SpawnConfigOptions {
                version: SpawnConfigVersion::V2,
                model_precedence: SpawnModelPrecedence::RequestedOverrides,
                fork_mode: fork_mode.as_ref(),
                role_name: None,
                model: farm_config.model.as_deref(),
                reasoning_effort: farm_config.reasoning_effort.clone(),
                service_tier: None,
            },
        )
        .await
        .map_err(CodexErr::InvalidRequest)?;
        let mut config = prepared.config;
        apply_farm_instructions(&mut config, &instructions);
        let depth = next_thread_spawn_depth(&turn.session_source);
        if exceeds_thread_spawn_depth_limit(depth, config.agent_max_depth) {
            return Err(CodexErr::InvalidRequest(
                "farm exceeds the configured agent depth".to_owned(),
            )
            .into());
        }
        let parent_path = turn
            .session_source
            .get_agent_path()
            .unwrap_or_else(AgentPath::root);
        let agent_path = if let Some(name) = &request.task_name {
            parent_path.join(name).map_err(CodexErr::InvalidRequest)?;
            let agents = self
                .session
                .services
                .agent_control
                .child_agent_paths(self.session.thread_id)
                .await;
            let mut chosen = None;
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
                let stem = &name[..name.len().min(capacity)];
                let candidate = parent_path
                    .join(&format!("{stem}{tail}"))
                    .map_err(CodexErr::InvalidRequest)?;
                if agents.contains(&candidate) {
                    continue;
                }
                if db
                    .farm_request_id_for_path(self.session.thread_id, candidate.as_str())
                    .await
                    .map_err(|error| {
                        CodexErr::Fatal(format!("could not validate farm child path: {error}"))
                    })?
                    .is_none()
                {
                    chosen = Some(candidate);
                    break;
                }
            }
            chosen.ok_or_else(|| {
                CodexErr::InvalidRequest("No available named farm child path".into())
            })?
        } else {
            parent_path
                .join(&format!(
                    "farm_{}",
                    &request_uuid.simple().to_string()[..12]
                ))
                .map_err(CodexErr::InvalidRequest)?
        };
        if agent_path.as_str().len() > MAX_DELEGATION_RECEIPT_PATH_BYTES {
            return Err(CodexErr::InvalidRequest(
                "farm child path exceeds the bounded receipt limit".to_owned(),
            )
            .into());
        }
        let source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: self.session.thread_id,
            depth,
            agent_path: Some(agent_path.clone()),
            agent_nickname: None,
            agent_role: prepared.role_name,
        });
        self.session
            .services
            .agent_control
            .check_turn_admission(turn.multi_agent_version, &source)?;
        if db
            .farm_request_id_for_path(self.session.thread_id, agent_path.as_str())
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!("could not validate farm child path: {error}"))
            })?
            .is_some()
        {
            return Err(CodexErr::InvalidRequest(format!(
                "farm child path `{agent_path}` is already reserved by another delegation"
            ))
            .into());
        }
        match db
            .claim_farm_request(self.session.thread_id, &request_id, &payload_sha256)
            .await
            .map_err(|error| CodexErr::Fatal(format!("could not claim farm request: {error}")))?
        {
            FarmRequestClaim::Existing(record) => {
                return replay(&request_id, &payload_sha256, record);
            }
            FarmRequestClaim::Claimed => {}
        }

        let result = async {
            self.session
                .record_user_delegation(authorization)
                .await
                .map_err(|error| {
                    CodexErr::Fatal(format!("could not persist user authorization: {error}"))
                })?;
            let (child, _) = self
                .session
                .services
                .agent_control
                .spawn(SpawnRequest {
                    caller: self.session.thread_id,
                    config,
                    input: AgentInput::UserInput(request.input),
                    source,
                    options: SpawnAgentOptions {
                        user_request: Some(UserSpawnRequest {
                            request_id: request_id.clone(),
                            instructions,
                            parent_developer_instructions: turn.developer_instructions.clone(),
                            fork_history,
                        }),
                        fork_mode,
                        parent_thread_id: Some(self.session.thread_id),
                        environments: Some(turn.initial_environments.clone()),
                        cyber_access_program: turn.cyber_access_program,
                        ..Default::default()
                    },
                })
                .await?;
            let receipt = FarmReceipt {
                child_thread_id: child.thread_id,
                agent_path: agent_path.to_string(),
            };
            self.session
                .record_conversation_items(
                    &turn,
                    turn.model_info(),
                    &[ContextualUserFragment::into(UserDelegationReceipt {
                        task_preview: task,
                        agent_path: receipt.agent_path.clone(),
                    })],
                )
                .await;
            self.flush_rollout().await.map_err(|error| {
                CodexErr::Fatal(format!("could not persist delegation receipt: {error}"))
            })?;
            db.accept_farm_request(self.session.thread_id, &request_id)
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
                let child = db
                    .farm_request(self.session.thread_id, &request_id)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|record| record.child_thread_id.zip(record.agent_path))
                    .map(|(child_thread_id, agent_path)| FarmReceipt {
                        child_thread_id,
                        agent_path,
                    });
                Err(FarmError::Uncertain {
                    request_id,
                    child,
                    message: format!(
                        "Delegation outcome is uncertain: {error}. The child may already be running; inspect the agent roster. This submission will not be executed again."
                    ),
                })
            }
        }
    }
}
