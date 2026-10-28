//! Initial base instructions, recorded in history for incremental-tools windows.

use super::ContextualUserFragment;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::input_text;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BaseInstructionsFragment(pub(crate) String);

impl BaseInstructionsFragment {
    pub(crate) const KIND: &str = "model.base_instructions";

    /// Reads only a dedicated, harness-classified base item, including an explicitly empty base.
    pub(crate) fn text_from_item(item: &ResponseItem) -> Option<&str> {
        let ResponseItem::Message {
            role,
            content,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = item
        else {
            return None;
        };
        match (
            role.as_str(),
            content.as_slice(),
            metadata.content_item_kinds.as_deref(),
        ) {
            ("developer", [ContentItem::InputText { text }], Some([kind]))
                if kind.as_str() == Self::KIND =>
            {
                Some(text)
            }
            _ => None,
        }
    }

    /// Identifies only a developer text part with a fully aligned harness kind array.
    pub(crate) fn part_index(item: &ResponseItem) -> Option<usize> {
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
        kinds.iter().zip(content).position(|(kind, content)| {
            kind.as_str() == Self::KIND && matches!(content, ContentItem::InputText { .. })
        })
    }

    pub(crate) fn text(item: &ResponseItem) -> Option<&str> {
        let part = Self::part_index(item)?;
        let ResponseItem::Message { content, .. } = item else {
            return None;
        };
        match &content[part] {
            ContentItem::InputText { text } => Some(text),
            _ => unreachable!("part_index only identifies input text"),
        }
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
            if metadata
                .content_item_metadata
                .as_ref()
                .is_some_and(|parts| parts.len() != content.len())
            {
                continue;
            }
            kinds.remove(part);
            if let Some(parts) = &mut metadata.content_item_metadata {
                parts.remove(part);
            }
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

    fn body(&self) -> ContentItem {
        input_text(self.0.clone())
    }
}
