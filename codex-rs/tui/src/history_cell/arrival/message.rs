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

pub(super) fn body(
    item: &ArrivalItem,
    width: u16,
) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
    use super::super::delegation::render_message_body;
    use super::super::output_provenance::OutputMode;
    use super::super::output_provenance::OutputProvenance;
    use super::super::output_provenance::client_notice;
    use super::super::output_provenance::context_sources;
    use super::super::output_provenance::mark_lines;
    let payload = envelope(item).map(|envelope| envelope.payload);
    match &item.source {
        ArrivalSource::Captured { blocks } => {
            let mut lines = Vec::new();
            for (index, block) in blocks.iter().enumerate() {
                if blocks.len() > 1 {
                    lines.push(client_notice(&format!("Text block {}", index + 1)));
                }
                let mut body = render_message_body(
                    if index == 0 {
                        payload.unwrap_or(block)
                    } else {
                        block
                    },
                    width,
                );
                mark_lines(&mut body, OutputProvenance::Captured);
                lines.extend(body);
            }
            lines
        }
        ArrivalSource::Legacy => {
            let mut lines = vec![client_notice(
                "Legacy arrival — exact context provenance was not recorded",
            )];
            if let Some(text) = payload.or(item.text.as_deref()) {
                let mut body = render_message_body(text, width);
                for source in body.iter_mut().filter_map(|line| line.source.as_mut()) {
                    source.bracket = None;
                }
                mark_lines(&mut body, OutputProvenance::Client);
                lines.extend(body);
            }
            lines
        }
        ArrivalSource::Unavailable => {
            vec![client_notice("Readable context capture unavailable")]
        }
        ArrivalSource::ToolDeclarations { text } => context_sources(
            &[codex_protocol::ContextSource::ToolDeclarations { text: text.clone() }],
            OutputMode::Raw,
        ),
    }
}
