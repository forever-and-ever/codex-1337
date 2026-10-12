use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ResponseItem;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BaseInstructionsFragment(pub(crate) String);

impl BaseInstructionsFragment {
    pub(crate) const KIND: &str = "model.base_instructions";

    pub(crate) fn matches_item(item: &ResponseItem) -> bool {
        let ResponseItem::Message {
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = item
        else {
            return false;
        };
        metadata
            .content_item_kinds
            .as_ref()
            .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Self::KIND))
    }

    pub(crate) fn text(item: &ResponseItem) -> Option<&str> {
        let ResponseItem::Message {
            role,
            content,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = item
        else {
            return None;
        };
        if role != "developer" {
            return None;
        }
        let kinds = metadata
            .content_item_kinds
            .as_ref()
            .filter(|kinds| kinds.len() == content.len())?;
        kinds.iter().zip(content).find_map(|(kind, content)| {
            if kind.as_str() == Self::KIND
                && let codex_protocol::models::ContentItem::InputText { text } = content
            {
                Some(text.as_str())
            } else {
                None
            }
        })
    }

    pub(crate) fn take_prefix(items: &mut Vec<ResponseItem>) -> Option<String> {
        Self::take_prefix_from(items, |item| item)
    }
    pub(crate) fn take_annotated_prefix(
        items: &mut Vec<codex_history::ResponseItemEnvelope>,
    ) -> Option<String> {
        Self::take_prefix_from(items, |item| &mut item.item)
    }

    /// Projects the immutable first declaration into protocols with a separate system field.
    /// Later amendments stay in chronological history, at their original authority.
    fn take_prefix_from<T>(
        items: &mut Vec<T>,
        mut body: impl FnMut(&mut T) -> &mut ResponseItem,
    ) -> Option<String> {
        for index in 0..items.len() {
            let ResponseItem::Message {
                role,
                content,
                internal_chat_message_metadata_passthrough: Some(metadata),
                ..
            } = body(&mut items[index])
            else {
                continue;
            };
            if role != "developer" {
                continue;
            }
            let Some(kinds) = metadata
                .content_item_kinds
                .as_mut()
                .filter(|kinds| kinds.len() == content.len())
            else {
                continue;
            };
            let Some(part) = kinds
                .iter()
                .zip(content.iter())
                .position(|(kind, content)| {
                    kind.as_str() == Self::KIND
                        && matches!(
                            content,
                            codex_protocol::models::ContentItem::InputText { .. }
                        )
                })
            else {
                continue;
            };
            kinds.remove(part);
            let codex_protocol::models::ContentItem::InputText { text } = content.remove(part)
            else {
                unreachable!()
            };
            if content.is_empty() {
                items.remove(index);
            }
            return Some(text);
        }
        None
    }
}

impl ContextualUserFragment for BaseInstructionsFragment {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind(Self::KIND.to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> String {
        self.0.clone()
    }
}
