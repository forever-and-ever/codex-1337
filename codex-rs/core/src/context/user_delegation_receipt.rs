use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

/// Model-visible routing receipt, separate from the retained genuine user assignment.
pub(crate) struct UserDelegationReceipt {
    pub(crate) task_preview: String,
    pub(crate) agent_path: String,
}

pub(crate) const MAX_DELEGATION_RECEIPT_PATH_BYTES: usize = 384;

impl ContextualUserFragment for UserDelegationReceipt {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("multi_agent.user_delegation_receipt".to_owned())
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<user_delegation_receipt>", "</user_delegation_receipt>")
    }

    fn body(&self) -> codex_protocol::models::ContentItem {
        let preview = &self.task_preview[..self.task_preview.floor_char_boundary(64)];
        let path = &self.agent_path;
        codex_protocol::models::input_text(format!(
            "\nUser delegated {} to {path}; it is already running. Do not duplicate it.\n",
            serde_json::json!(preview)
        ))
    }
}
