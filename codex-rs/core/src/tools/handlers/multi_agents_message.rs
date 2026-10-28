//! Agent work-order arguments preserve encrypted and explicitly plaintext delivery.

use crate::agent::types::AgentMessage;
use crate::function_tool::FunctionCallError;
use crate::tools::context::AgentMessageEncoding;
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
                required: Some([field.into()].into()),
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
    let (content, encoding) = match (message, plaintext_message) {
        (Some(message), None) => {
            let encoding = match source {
                ToolCallSource::DirectAgentMessage(encoding) => *encoding,
                // Nested arguments are JS values, not schema-encrypted Responses arguments.
                ToolCallSource::CodeMode { .. } => AgentMessageEncoding::Plaintext,
                ToolCallSource::Direct => {
                    return Err(FunctionCallError::RespondToModel(
                        "Agent message is missing its argument encoding".into(),
                    ));
                }
            };
            (message, encoding)
        }
        (None, Some(message)) => (message, AgentMessageEncoding::Plaintext),
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
    Ok(match encoding {
        AgentMessageEncoding::Plaintext => AgentMessage::Plaintext(content),
        AgentMessageEncoding::Encrypted => AgentMessage::Encrypted(content),
    })
}
