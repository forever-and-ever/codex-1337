//! Read-only accounting uses the captured prefix, never recaptures capabilities.
use super::Session;
use super::preamble::InjectedText;
use super::preamble::PreambleTools;
use super::preamble::census;
use crate::context::BaseInstructionsFragment;
use crate::context::is_contextual_user_fragment;
use crate::context::world_state::PreviousSectionState;
use crate::context::world_state::SectionTransition;
use crate::context::world_state::WorldStateSection;
use codex_protocol::ContextInventory;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::borrow::Cow;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct InspectionPrefix {
    pub(crate) instructions: String,
    pub(crate) tools: Vec<Value>,
}

impl WorldStateSection for InspectionPrefix {
    const ID: &'static str = "inspection_prefix";
    type Snapshot = Self;
    fn render_diff(&self, _: PreviousSectionState<'_, Self>) -> SectionTransition<Self> {
        (Some(self.clone()), Vec::new())
    }
}

impl Session {
    pub(crate) async fn context_inventory(&self) -> Result<ContextInventory, String> {
        let (prefix, items) = {
            let state = self.state.lock().await;
            let checkpoint = state
                .history
                .world_state_checkpoint()
                .ok_or("Context is not available before the first model request.")?;
            let prefix: InspectionPrefix = serde_json::from_value(
                checkpoint.state.get(InspectionPrefix::ID).cloned()
                    .ok_or("This older window has no saved prefix; inspect it after the next model request.")?
            ).map_err(|error| format!("Cannot read the saved context prefix: {error}"))?;
            (
                prefix,
                state
                    .history
                    .raw_items()
                    .filter(|item| match item {
                        ResponseItem::AdditionalTools { .. } => true,
                        ResponseItem::Message { role, content, .. } => {
                            role == "developer"
                                || role == "system"
                                || content.iter().any(is_contextual_user_fragment)
                        }
                        _ => false,
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        tokio::task::spawn_blocking(move || {
            let mut instructions = prefix.instructions;
            let mut tools = prefix.tools;
            let mut context = Vec::new();
            let mut items = items;
            if let Some(text) = BaseInstructionsFragment::take_prefix(&mut items)
                && instructions.is_empty()
            {
                instructions = text;
            }
            for item in items {
                match item {
                    ResponseItem::AdditionalTools {
                        tools: declarations,
                        ..
                    } => tools.extend(declarations.iter().cloned()),
                    ResponseItem::Message { role, content, .. } => {
                        for part in content {
                            if (role == "developer"
                                || role == "system"
                                || is_contextual_user_fragment(&part))
                                && let ContentItem::InputText { text } = part
                            {
                                context.push(InjectedText {
                                    role: role.clone(),
                                    text,
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            let tools = PreambleTools::History(Cow::Owned(tools));
            let sources = super::preamble::context_sources(&instructions, &tools, &context);
            ContextInventory {
                parts: census(&instructions, &tools, context),
                raw: String::new(),
                sources,
            }
        })
        .await
        .map_err(|error| format!("Context accounting failed: {error}"))
    }
}
