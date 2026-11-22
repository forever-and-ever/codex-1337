//! Direct user delegation through the ordinary shared child-spawn path.

use super::api::AgentInput;
use super::api::SpawnRequest;
use super::child_config::SpawnConfigOptions;
use super::child_config::SpawnConfigVersion;
use super::child_config::SpawnModelPrecedence;
use super::child_config::build_agent_spawn_config;
use super::child_config::prepare_agent_spawn_config_from_selection;
use super::child_config::resolve_spawn_agent_model;
use super::exceeds_thread_spawn_depth_limit;
use super::history::load_agent_model_context;
use super::next_thread_spawn_depth;
use super::types::SpawnAgentForkMode;
use super::types::SpawnAgentOptions;
use super::types::UserSpawnRequest;
use crate::codex_thread::CodexThread;
use crate::session::thread_settings;
use codex_history::RetainedUserMessage;
use codex_history::UserInputOrigin;
use codex_protocol::AgentPath;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
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
    /// Keep full history only when the selected executor retains the parent provider.
    Automatic,
    FullHistory,
    Fresh,
}

pub struct FarmRequest {
    pub request_id: String,
    pub mode: FarmMode,
    pub input: Vec<UserInput>,
    pub task_name: Option<String>,
}

pub struct FarmSpawnRequest {
    pub spawn: SpawnRequest,
    /// Authoritative captured user boundary, transferred into spawn options by the controller.
    pub user_request: UserSpawnRequest,
    pub payload_sha256: String,
    pub task_name: Option<String>,
    pub authorization: RetainedUserMessage,
    pub task_preview: String,
    pub turn: Arc<crate::TurnContext>,
}

pub use codex_state::FarmReceipt;

/// Only a definitive rejection may safely be edited and submitted as a different request.
#[derive(Debug)]
pub enum FarmError {
    /// Durable identity-checked proof that this submission allocated no child.
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

pub(crate) fn replay(
    request_id: &str,
    payload_sha256: &str,
    record: FarmRequestRecord,
) -> Result<FarmReceipt, FarmError> {
    use codex_state::FarmRequestState;
    let child = match &record.state {
        FarmRequestState::Allocated(child) | FarmRequestState::Accepted(child) => {
            Some(child.clone())
        }
        _ => None,
    };
    if record.payload_sha256 != payload_sha256 {
        return Err(FarmError::IdentityMismatch {
            request_id: request_id.to_owned(),
            child,
        });
    }
    match record.state {
        FarmRequestState::Accepted(child) => Ok(child),
        FarmRequestState::Rejected(rejection) => Err(FarmError::Rejected(rejection.into())),
        _ => Err(FarmError::Uncertain {
            request_id: request_id.to_owned(),
            child,
            message: "Delegation outcome is uncertain. Inspect the agent roster before taking further action. This submission will not be executed again.".to_owned(),
        }),
    }
}

pub(crate) async fn reject_unclaimed(
    db: &codex_state::StateRuntime,
    parent: codex_protocol::ThreadId,
    request_id: &str,
    payload_sha256: &str,
    error: CodexErr,
) -> Result<FarmReceipt, FarmError> {
    let record = db
        .reject_unclaimed_farm_request(
            parent,
            request_id,
            payload_sha256,
            &codex_state::FarmRejection::from(&error),
        )
        .await
        .map_err(|error| FarmError::Uncertain {
            request_id: request_id.to_owned(),
            child: None,
            message: format!("could not confirm farm rejection: {error}; replay this request ID"),
        })?;
    replay(request_id, payload_sha256, record)
}

impl CodexThread {
    /// Accepts genuine user input without starting, steering, or interrupting this parent.
    /// Call only from a user-facing host action, never from an agent's tool request.
    pub async fn farm(&self, request: FarmRequest) -> Result<FarmReceipt, FarmError> {
        let unverified = |message| FarmError::Uncertain {
            request_id: request.request_id.clone(),
            child: None,
            message,
        };
        let request_uuid = Uuid::parse_str(&request.request_id)
            .map_err(|_| unverified("farm requestId must be a UUID".to_owned()))?;
        let request_id = request_uuid.to_string();
        // Keep ordinary farm hashes compatible with receipts created before named farms.
        let payload = match &request.task_name {
            Some(name) => serde_json::to_vec(&(request.mode, &request.input, name)),
            None => serde_json::to_vec(&(request.mode, &request.input)),
        }
        .map_err(|error| unverified(format!("could not encode farm submission: {error}")))?;
        let payload_sha256 = format!("{:x}", Sha256::digest(payload));
        let db = self
            .state_db()
            .ok_or_else(|| unverified("farm requires durable thread state".to_owned()))?;
        if let Some(record) = db
            .farm_request(self.session.thread_id, &request_id)
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
        let prepared = Box::pin(async {
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
                return Err(CodexErr::InvalidRequest(
                    "farm requires a nonempty task".to_owned(),
                ));
            }
            let command = match request.mode {
                FarmMode::Automatic => "/farm_auto",
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
                ));
            }

            // Materialization checkpoints the initial account binding under the same
            // nonreentrant permit used below. It must finish before the snapshot boundary.
            self.ensure_rollout_materialized().await;
            // Settings, ready environment bindings, and the ordinary stored fork input are
            // captured under the checkpoint/settings boundary, never a whole-turn/tool lock.
            let (turn, fork_history) = {
                let _boundary = thread_settings::acquire_persistence_lock(&self.session).await;
                let turn = Box::pin(self.session.new_inject_items_context()).await;
                self.flush_rollout()
                    .await
                    .map_err(|error| CodexErr::Fatal(format!("could not flush parent: {error}")))?;
                // Automatic captures locally before catalog I/O so settings and history
                // retain one checkpoint boundary. A fresh route discards both the history
                // and any read error; mandatory persistence errors above still reject it.
                let history = if request.mode == FarmMode::Fresh {
                    None
                } else {
                    Some(
                        async {
                            let state = self.session.services.local_agent_runtime.upgrade()?;
                            Ok::<_, CodexErr>(Arc::new(
                                load_agent_model_context(
                                    &state,
                                    self.session.thread_id,
                                    turn.history_mode,
                                )
                                .await?
                                .items,
                            ))
                        }
                        .await,
                    )
                };
                (turn, history)
            };
            if turn.multi_agent_version != MultiAgentVersion::V2 {
                return Err(CodexErr::InvalidRequest(
                    "farm requires multi-agent v2".to_owned(),
                ));
            }
            if turn.config.ephemeral || self.session.live_thread().is_none() {
                return Err(CodexErr::InvalidRequest(
                    "farm requires a persistent parent conversation".to_owned(),
                ));
            }
            let farm_config = &turn.config.wrangler.farm;
            let executor = farm_config.executor(&turn.config.model_provider_id);
            let selection = {
                let parent_config = build_agent_spawn_config(
                    &self.session.get_base_instructions().await,
                    &turn,
                    &turn.initial_settings,
                )
                .map_err(CodexErr::InvalidRequest)?;
                resolve_spawn_agent_model(
                    &self.session,
                    &turn,
                    &parent_config,
                    executor.model.as_deref(),
                )
                .await
                .map_err(CodexErr::InvalidRequest)?
            };
            let fork_mode = match request.mode {
                FarmMode::FullHistory => Some(SpawnAgentForkMode::FullHistory),
                FarmMode::Fresh => None,
                FarmMode::Automatic => selection
                    .as_ref()
                    .is_none_or(|selection| selection.provider_id == turn.config.model_provider_id)
                    .then_some(SpawnAgentForkMode::FullHistory),
            };
            #[expect(clippy::expect_used, reason = "only non-fresh modes select full history, and both captured a history result before model selection")]
            let fork_history = if fork_mode.is_some() {
                Some(fork_history.expect("history mode captures parent history")?)
            } else {
                None
            };
            let extra = farm_config.instructions.as_deref().unwrap_or_default();
            if extra.len() > MAX_EXTRA_INSTRUCTIONS_BYTES {
                return Err(CodexErr::InvalidRequest(
                    "wrangler.farm.instructions exceeds 512 bytes".to_owned(),
                ));
            }
            let instructions = if extra.is_empty() {
                CHILD_INSTRUCTIONS.to_owned()
            } else {
                format!("{CHILD_INSTRUCTIONS}\n\n{extra}")
            };
            let prepared = Box::pin(prepare_agent_spawn_config_from_selection(
                &self.session,
                &turn,
                &turn.initial_settings,
                SpawnConfigOptions {
                    version: SpawnConfigVersion::V2,
                    model_precedence: SpawnModelPrecedence::RequestedOverrides,
                    fork_mode: fork_mode.as_ref(),
                    role_name: None,
                    model: executor.model.as_deref(),
                    reasoning_effort: executor.reasoning_effort,
                    service_tier: None,
                },
                selection,
            ))
            .await
            .map_err(CodexErr::InvalidRequest)?;
            let mut config = prepared.config;
            apply_farm_instructions(&mut config, &instructions);
            let depth = next_thread_spawn_depth(&turn.session_source);
            if exceeds_thread_spawn_depth_limit(depth, config.agent_max_depth) {
                return Err(CodexErr::InvalidRequest(
                    "farm exceeds the configured agent depth".to_owned(),
                ));
            }
            let parent_path = turn
                .session_source
                .get_agent_path()
                .unwrap_or_else(AgentPath::root);
            let agent_path = parent_path
                .join(&request.task_name.clone().unwrap_or_else(|| {
                    format!("farm_{}", &request_uuid.simple().to_string()[..12])
                }))
                .map_err(CodexErr::InvalidRequest)?;
            let source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: self.session.thread_id,
                depth,
                agent_path: Some(agent_path),
                agent_nickname: None,
                agent_role: prepared.role_name,
            });
            self.session
                .services
                .agent_control
                .check_turn_admission(turn.multi_agent_version, &source)?;
            let input = crate::codex_thread::resolve_client_inputs(request.input).await?;
            Ok::<_, CodexErr>(FarmSpawnRequest {
                spawn: SpawnRequest {
                    caller: self.session.thread_id,
                    config,
                    input: AgentInput::UserInput(input),
                    source,
                    options: SpawnAgentOptions {
                        fork_mode,
                        parent_thread_id: Some(self.session.thread_id),
                        environments: Some(turn.initial_environments.clone()),
                        cyber_access_program: turn.cyber_access_program,
                        ..Default::default()
                    },
                },
                user_request: UserSpawnRequest {
                    request_id: request_id.clone(),
                    instructions,
                    parent_developer_instructions: turn.developer_instructions.clone(),
                    fork_history,
                },
                payload_sha256: payload_sha256.clone(),
                task_name: request.task_name,
                authorization,
                task_preview: task,
                turn,
            })
        })
        .await;
        let spawn = match prepared {
            Ok(spawn) => spawn,
            Err(error) => {
                return reject_unclaimed(
                    &db,
                    self.session.thread_id,
                    &request_id,
                    &payload_sha256,
                    error,
                )
                .await;
            }
        };
        self.session.services.agent_control.farm_spawn(spawn).await
    }
}
