//! Agent data and execution reservations shared by controllers and their callers.
//! Backend operations and local admission policy live in the implementations.

use crate::context::MultiAgentRoleInstructions;
use crate::environment_selection::TurnEnvironmentSnapshot;
use codex_history::RolloutItem;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::turn_input::CyberAccessProgram;
use std::collections::HashMap;
use std::sync::Arc;

/// Registry identity shared by loaded and unloaded agents.
/// Registered agents have an `agent_id`; a reserved spawn can still be awaiting its ID.
#[derive(Clone, Debug, Default)]
pub struct AgentMetadata {
    pub agent_id: Option<ThreadId>,
    pub agent_path: Option<AgentPath>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnAgentForkMode {
    FullHistory,
}

/// Whether a spawn program was chosen for the child or copied from its parent turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpawnProgramSource {
    #[default]
    Explicit,
    ParentTurn,
}

#[derive(Clone, Debug, Default)]
pub struct SpawnAgentOptions {
    pub request_country: codex_protocol::RequestCountry,
    pub fork_parent_spawn_call_id: Option<String>,
    /// Host-captured boundary for a direct user action, never an assistant tool invocation.
    pub user_request: Option<UserSpawnRequest>,
    pub fork_mode: Option<SpawnAgentForkMode>,
    pub parent_thread_id: Option<ThreadId>,
    pub parent_turn_id: Option<String>,
    /// Attribute delegated usage to the turn that initiated it.
    pub turn_trigger: Option<String>,
    pub root_turn_id: Option<String>,
    /// Client attribution captured from the turn that requested the child.
    pub inherited_client_metadata: HashMap<String, String>,
    /// Inheritable bindings captured by the step that requested the child.
    pub environments: Option<TurnEnvironmentSnapshot>,
    /// Plugin choices from the turn requesting the child. Omission inherits the parent's
    /// current choices; an explicit list, including an empty one, overrides them and history.
    pub disabled_plugin_ids: Option<Vec<String>>,
    pub multi_agent_v2_usage_hints: Option<ResolvedMultiAgentV2UsageHints>,
    pub cyber_access_program: Option<CyberAccessProgram>,
    /// Parent selections apply to the first turn and may need adapting to the child's model.
    pub cyber_access_program_source: SpawnProgramSource,
}

#[derive(Clone, Debug)]
pub struct UserSpawnRequest {
    pub request_id: String,
    pub instructions: String,
    pub parent_developer_instructions: Option<String>,
    /// The ordinary stored fork input, captured together with the parent's current settings.
    pub fork_history: Option<Arc<Vec<RolloutItem>>>,
}

/// Identity and status observed from a loaded agent, without a handle to its runtime.
#[derive(Clone, Debug)]
pub struct LiveAgent {
    pub thread_id: ThreadId,
    pub metadata: AgentMetadata,
    pub status: AgentStatus,
}

/// A retained family member; absence of status means unloaded, not retired.
#[derive(Debug, serde::Serialize)]
pub struct ListedAgent {
    pub agent_name: String,
    pub agent_status: Option<AgentStatus>,
    #[serde(flatten)]
    pub tier: Option<AgentTierStatus>,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct AgentTierStatus {
    pub service_tier: String,
    pub effective_service_tier: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ResolvedMultiAgentV2UsageHints {
    pub root: Option<MultiAgentRoleInstructions>,
    pub subagent: Option<MultiAgentRoleInstructions>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MessageDeliveryMode {
    /// Deliver attributed mail, waking an idle recipient without assigning a new task.
    Ordinary,
    /// Deliver to the active turn or start work if the agent is idle.
    TriggerTurn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentMessageEncoding {
    Plaintext,
    Encrypted,
}

/// Keeps model-provided encrypted content distinct from text that needs a context wrapper.
pub enum AgentMessage {
    Plaintext(String),
    Encrypted(String),
}

/// Holds a backend-owned reservation until the turn ends or is cancelled.
///
/// The permit's destructor releases capacity or arranges backend cleanup. Remote backends
/// must also recover reservations after worker loss, when no Rust destructor can run.
#[must_use = "hold the execution guard for the lifetime of the admitted turn"]
pub struct AgentExecutionGuard {
    _permit: Box<dyn Send + Sync>,
}

impl AgentExecutionGuard {
    pub fn new(permit: impl Send + Sync + 'static) -> Self {
        Self {
            _permit: Box::new(permit),
        }
    }
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
