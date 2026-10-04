use super::invalid_request;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnSteerParams;
use codex_app_server_protocol::UserInput;
use codex_core::CodexThread;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;

pub(super) const DIRECT_INPUT_TO_MULTI_AGENT_V2_SUBAGENT_ERROR: &str =
    "direct app-server input is not allowed for multi-agent v2 sub-agents";

/// Mirrors the ownership policy in request validation and thread capability responses.
pub(super) fn can_accept_direct_input(
    multi_agent_version: Option<MultiAgentVersion>,
    session_source: &SessionSource,
) -> bool {
    multi_agent_version != Some(MultiAgentVersion::V2)
        || !matches!(
            session_source,
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
        )
}

pub(super) async fn ensure_direct_input_allowed(
    thread: &CodexThread,
) -> Result<(), JSONRPCErrorError> {
    let config_snapshot = thread.config_snapshot().await;
    if !can_accept_direct_input(
        thread.multi_agent_version(),
        &config_snapshot.session_source,
    ) {
        return Err(invalid_request(
            DIRECT_INPUT_TO_MULTI_AGENT_V2_SUBAGENT_ERROR,
        ));
    }

    Ok(())
}

/// Child text admission is separate from ownership: no settings, context or machine input.
pub(super) fn validate_child_text_start(params: &TurnStartParams) -> Result<(), JSONRPCErrorError> {
    let allowed = TurnStartParams {
        thread_id: params.thread_id.clone(),
        client_user_message_id: params.client_user_message_id.clone(),
        input: params.input.clone(),
        ..Default::default()
    };
    if params != &allowed || !is_text_input(&params.input) {
        return Err(invalid_request(DIRECT_INPUT_TO_MULTI_AGENT_V2_SUBAGENT_ERROR));
    }
    Ok(())
}

pub(super) fn validate_child_text_steer(params: &TurnSteerParams) -> Result<(), JSONRPCErrorError> {
    let allowed = TurnSteerParams {
        thread_id: params.thread_id.clone(),
        client_user_message_id: params.client_user_message_id.clone(),
        input: params.input.clone(),
        expected_turn_id: params.expected_turn_id.clone(),
        ..Default::default()
    };
    if params != &allowed || !is_text_input(&params.input) {
        return Err(invalid_request(DIRECT_INPUT_TO_MULTI_AGENT_V2_SUBAGENT_ERROR));
    }
    Ok(())
}

fn is_text_input(input: &[UserInput]) -> bool {
    !input.is_empty() && input.iter().all(|item| matches!(item, UserInput::Text { .. }))
}
