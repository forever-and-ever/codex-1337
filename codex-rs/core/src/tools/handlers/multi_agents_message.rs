//! Agent work-order arguments preserve encrypted and explicitly plaintext delivery.

use crate::agent::types::AgentMessage;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolCallSource;
use codex_tools::JsonSchema;
use std::collections::BTreeMap;

pub(super) fn agent_message_parameters(
    mut properties: BTreeMap<String, JsonSchema>,
    required: Vec<String>,
) -> JsonSchema {
    properties.insert("message".into(), JsonSchema::string(Some(
        "Task or message. Use plaintext_message instead for another provider or account; provide exactly one of these fields.".into(),
    )).with_encrypted());
    properties.insert("plaintext_message".into(), JsonSchema::string(Some(
        "Unencrypted task or message for cross-provider or cross-account delivery. For example: plaintext_message: \"Inspect the failing test\". Provide exactly one of message or plaintext_message.".into(),
    )));
    let mut parameters = JsonSchema::object(properties, Some(required), Some(false.into()));
    parameters.one_of = Some(
        ["message", "plaintext_message"]
            .map(|field| JsonSchema {
                required: Some(vec![field.into()]),
                ..Default::default()
            })
            .into(),
    );
    parameters
}

pub(super) fn parse_agent_message(
    message: Option<String>,
    plaintext_message: Option<String>,
    source: &ToolCallSource,
) -> Result<AgentMessage, FunctionCallError> {
    let (content, plaintext) = match (message, plaintext_message) {
        (Some(message), None) => (
            message,
            matches!(source, ToolCallSource::DirectPlaintextMessage),
        ),
        (None, Some(message)) => (message, true),
        _ => {
            return Err(FunctionCallError::RespondToModel(
                "Provide exactly one of message or plaintext_message".into(),
            ));
        }
    };
    if content.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "Empty message can't be sent to an agent".into(),
        ));
    }
    Ok(if plaintext {
        AgentMessage::Plaintext(content)
    } else {
        AgentMessage::Encrypted(content)
    })
}
