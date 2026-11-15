//! Per-agent routing selections, captured atomically before each inference step.

use super::LocalAgentControl;
use codex_protocol::ThreadId;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::openai_models::ModelInfo;
use std::collections::HashMap;

pub(crate) use crate::agent::types::AgentTierStatus;

pub(crate) fn normalize_agent_service_tier(
    requested: &str,
    model: &ModelInfo,
    enabled: bool,
) -> Result<Option<String>, String> {
    let requested = requested.trim();
    if requested == "inherit" {
        return Ok(None);
    }
    let requested = if requested == "fast" {
        ServiceTier::Fast.request_value()
    } else {
        requested
    };
    if requested == SERVICE_TIER_DEFAULT_REQUEST_VALUE
        || (enabled && model.supports_service_tier(requested))
    {
        return Ok(Some(requested.to_owned()));
    }
    Err(format!(
        "Service tier `{requested}` is unavailable for model `{}`; use `inherit`, `default`, or an advertised tier.",
        model.slug
    ))
}

#[derive(Clone, Default)]
pub(super) struct TierTree {
    root: Option<String>,
    agents: HashMap<ThreadId, TierBinding>,
}

#[derive(Clone)]
struct TierBinding {
    parent: ThreadId,
    /// None follows the immediate parent's requested tier, not its model-filtered result.
    selected: Option<String>,
}

impl TierTree {
    fn requested(&self, mut agent: ThreadId) -> Option<String> {
        for _ in 0..=self.agents.len() {
            let Some(binding) = self.agents.get(&agent) else {
                return self.root.clone();
            };
            if let Some(selected) = &binding.selected {
                return Some(selected.clone());
            }
            agent = binding.parent;
        }
        unreachable!("agent tier inheritance contains a cycle")
    }
}

impl LocalAgentControl {
    pub(crate) fn root_service_tier(&self) -> Option<String> {
        self.runtime.service_tiers.load().root.clone()
    }

    pub(crate) fn set_root_service_tier(&self, service_tier: Option<String>) {
        self.runtime.service_tiers.rcu(|current| {
            let mut next = (**current).clone();
            next.root = service_tier.clone();
            next
        });
    }

    pub(crate) fn set_agent_tier_binding(
        &self,
        agent: ThreadId,
        parent: ThreadId,
        selected: Option<String>,
    ) {
        self.runtime.service_tiers.rcu(|current| {
            let mut next = (**current).clone();
            next.agents.insert(
                agent,
                TierBinding {
                    parent,
                    selected: selected.clone(),
                },
            );
            next
        });
    }

    pub(crate) fn selected_agent_service_tier(&self, mut agent: ThreadId) -> Option<String> {
        let tiers = self.runtime.service_tiers.load();
        for _ in 0..=tiers.agents.len() {
            let binding = tiers.agents.get(&agent)?;
            if let Some(selected) = &binding.selected {
                return Some(selected.clone());
            }
            agent = binding.parent;
        }
        unreachable!("agent tier inheritance contains a cycle")
    }

    pub(crate) fn agent_service_tier(&self, agent: ThreadId) -> Option<String> {
        self.runtime.service_tiers.load().requested(agent)
    }

    pub(crate) fn agent_request_service_tier(
        &self,
        agent: ThreadId,
        model: &ModelInfo,
        enabled: bool,
    ) -> Result<Option<String>, String> {
        let tree = self.runtime.service_tiers.load();
        // A later model switch must not silently discard an explicit routing choice.
        if let Some(selected) = tree
            .agents
            .get(&agent)
            .and_then(|binding| binding.selected.as_deref())
        {
            normalize_agent_service_tier(selected, model, enabled)?;
        }
        Ok(tree.requested(agent))
    }

    pub(crate) async fn select_agent_service_tier(
        &self,
        caller: ThreadId,
        target: ThreadId,
        requested: &str,
    ) -> CodexResult<AgentTierStatus> {
        self.runtime.ensure_agent_known(target)?;
        let descendant = {
            let tree = self.runtime.service_tiers.load();
            let mut cursor = target;
            let mut found = false;
            for _ in 0..tree.agents.len() {
                let Some(binding) = tree.agents.get(&cursor) else {
                    break;
                };
                if binding.parent == caller {
                    found = true;
                    break;
                }
                cursor = binding.parent;
            }
            target != caller && found
        };
        if !descendant {
            return Err(CodexErr::InvalidRequest(
                "Service tiers may only be changed for your descendants.".to_owned(),
            ));
        }
        self.runtime
            .upgrade()?
            .get_thread(target)
            .await?
            .session
            .select_agent_service_tier(requested)
            .await
    }
}

#[cfg(test)]
#[path = "service_tier_tests.rs"]
mod tests;
