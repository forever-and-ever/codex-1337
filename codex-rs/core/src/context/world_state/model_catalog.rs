//! Keeps spawn model choices in append-only context instead of the tool schema.
//! Selectors and capabilities are complete; only descriptive prose is budgeted.
//! Catalog refreshes never rewrite earlier messages, and missing retained context
//! gets a fresh catalog.

use super::PreviousSectionState;
use super::SectionTransition;
use super::WorldStateHash;
use super::WorldStateSection;
use super::WorldStateUpdate;
use crate::agent::child_config::model_supports_multi_agent_backend;
use crate::agent::model_catalog::describe_agent_model;
use crate::context::ContextualUserFragment;
use crate::context::environment_context::push_xml_escaped_text;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ContentItemMetadata;
use codex_protocol::models::ContentItemNamespace;
use codex_protocol::models::input_text;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::protocol::MultiAgentVersion;

// Soft rendered budget: identities and capabilities must survive even if they exceed it.
const RENDERED_BYTES_BUDGET: usize = 1_000;
const MAX_DESCRIPTION_CHARS: usize = 250;

/// The current model choices, or an empty catalog to invalidate a retained listing.
#[derive(Clone, Default)]
pub(crate) struct ModelCatalogState {
    catalog: String,
    namespace: Option<ContentItemNamespace>,
}

impl ModelCatalogState {
    pub(crate) fn new(models: &[ModelPreset], multi_agent_version: MultiAgentVersion) -> Self {
        let mut models = models
            .iter()
            .filter(|model| model.show_in_picker)
            .filter(|model| model_supports_multi_agent_backend(model, multi_agent_version))
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.model.cmp(&right.model));
        if models.is_empty() {
            return Self {
                catalog: "No picker-visible model overrides are currently loaded.\n".to_string(),
                namespace: None,
            };
        }
        let compact = models
            .iter()
            .map(|model| {
                let mut escaped = String::new();
                push_xml_escaped_text(&mut escaped, &describe_agent_model(model, ""));
                escaped
            })
            .collect::<Vec<_>>();
        let (open, close) = Self::type_markers();
        let mandatory_bytes =
            open.len() + close.len() + 2 + compact.iter().map(|line| line.len() + 1).sum::<usize>();
        let mut prose_budget = RENDERED_BYTES_BUDGET.saturating_sub(mandatory_bytes);
        let mut catalog = String::new();
        for (model, compact) in models.into_iter().zip(compact) {
            let mut description = model
                .description
                .chars()
                .take(MAX_DESCRIPTION_CHARS)
                .collect::<String>();
            if model.description.chars().count() > MAX_DESCRIPTION_CHARS {
                description.push('…');
            }
            let mut detailed = String::new();
            push_xml_escaped_text(&mut detailed, &describe_agent_model(model, &description));
            let prose_bytes = detailed.len() - compact.len();
            if prose_bytes <= prose_budget {
                catalog.push_str(&detailed);
                prose_budget -= prose_bytes;
            } else {
                catalog.push_str(&compact);
            }
            catalog.push('\n');
        }
        Self {
            catalog,
            namespace: None,
        }
    }

    pub(crate) fn with_namespace(mut self, namespace: Option<ContentItemNamespace>) -> Self {
        self.namespace = namespace;
        self
    }
}

impl ContextualUserFragment for ModelCatalogState {
    fn content_metadata(&self) -> ContentItemMetadata {
        ContentItemMetadata::tool(self.namespace.clone())
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("multi_agent.model_catalog".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<model_catalog>", "</model_catalog>")
    }

    fn body(&self) -> ContentItem {
        input_text(if self.catalog.is_empty() {
            "\nThe previous spawn_agent model catalog no longer applies.\n".to_string()
        } else {
            format!("\n{}", self.catalog)
        })
    }
}

impl WorldStateSection for ModelCatalogState {
    const ID: &'static str = "model_catalog";
    type Snapshot = WorldStateHash;

    fn matches_legacy_fragment(role: &str, text: &str) -> bool {
        role == "developer" && Self::matches_text(text)
    }

    fn has_retained_fragment_matcher() -> bool {
        true
    }

    fn matches_retained_fragment(role: &str, text: &str) -> bool {
        Self::matches_legacy_fragment(role, text)
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> SectionTransition<Self::Snapshot> {
        let current = WorldStateHash::from_fragment(self);
        if matches!(previous, PreviousSectionState::Known(previous) if previous == &current)
            || self.catalog.is_empty() && matches!(previous, PreviousSectionState::Absent)
        {
            return (Some(current), Vec::new());
        }
        (
            Some(current),
            vec![WorldStateUpdate::fragment(self.clone())],
        )
    }
}
