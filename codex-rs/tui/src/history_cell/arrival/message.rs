//! Compact presentation of the harness envelope; the captured source remains intact.

use codex_app_server_protocol::ArrivalItem;
use codex_protocol::step_admission::ArrivalKind;
use codex_protocol::step_admission::ArrivalSource;

pub(super) struct Envelope<'a> {
    pub(super) message_type: &'a str,
    pub(super) payload: &'a str,
}

pub(super) fn envelope(item: &ArrivalItem) -> Option<Envelope<'_>> {
    if item.kind != ArrivalKind::InterAgent {
        return None;
    }
    let text = match &item.source {
        ArrivalSource::Captured { blocks } => blocks.first()?.as_str(),
        ArrivalSource::Legacy => item.text.as_deref()?,
        ArrivalSource::ToolDeclarations { .. } | ArrivalSource::Unavailable => return None,
    };
    let (message_type, rest) = text.strip_prefix("Message Type: ")?.split_once('\n')?;
    if message_type.is_empty()
        || !message_type
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
    {
        return None;
    }
    let (task, rest) = rest.strip_prefix("Task name: ")?.split_once('\n')?;
    if task.is_empty() {
        return None;
    }
    let (sender, rest) = rest.strip_prefix("Sender: ")?.split_once('\n')?;
    if sender != item.label {
        return None;
    }
    Some(Envelope {
        message_type,
        payload: rest.strip_prefix("Payload:\n")?,
    })
}
