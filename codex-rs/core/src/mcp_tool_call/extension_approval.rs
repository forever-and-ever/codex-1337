//! Native extensions use the existing policy and pending-approval machinery,
//! without manufacturing an MCP transport or trusting a frontend's identity.

use codex_mcp::McpServerKey;

use std::sync::Arc;

use codex_config::types::AppToolApproval;
use codex_protocol::protocol::McpInvocation;
use codex_protocol::protocol::ReviewDecision;
use codex_tools::FunctionCallError;
use codex_tools::ToolApproval;
use codex_tools::ToolApprovalMode;

use crate::tools::context::ToolInvocation;

use super::McpToolApproval;
use super::McpToolApprovalMetadata;
use super::McpToolApprovalPolicy;
use super::apply_mcp_tool_approval_decision;
use super::maybe_request_mcp_tool_approval;
use super::persistent_mcp_tool_approval_key;
use super::session_mcp_tool_approval_key;

pub(crate) async fn approve_extension_tool(
    call: &ToolInvocation,
    approval: ToolApproval,
) -> Result<(), FunctionCallError> {
    let config = &call.turn.config;
    let tool = &call.tool_name.name;
    let arguments = match &call.payload {
        codex_tools::ToolPayload::Function { arguments } => serde_json::from_str(arguments)
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("invalid tool arguments: {err}"))
            })?,
        _ => {
            return Err(FunctionCallError::Fatal(
                "approval requires function arguments".into(),
            ));
        }
    };
    let server_config = config.mcp_servers.get().get(&approval.namespace);
    let tool_config = server_config.and_then(|server| server.tools.get(tool));
    // Managed admission is independent of user tool policy. A transport-free
    // capability cannot manufacture the URL/command identity enterprise policy
    // requires. Retain an admitted legacy identity, or fail closed.
    if approval.namespace == "codex_tui"
        && let Some(requirements) = &config.config_layer_stack.requirements().mcp_servers
    {
        let requirement = requirements.value.get(&approval.namespace).ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "native coordination is denied: managed MCP requirements exclude codex_tui".into(),
            )
        })?;
        if !server_config.is_some_and(|server| server.matches_requirement(requirement)) {
            return Err(FunctionCallError::RespondToModel(
                "native coordination is denied: no admitted legacy identity satisfies the managed codex_tui URL/command requirement; user native policy is not a managed grant".into(),
            ));
        }
        if server_config.is_some_and(|server| {
            matches!(
                server.disabled_reason,
                Some(
                    codex_config::McpServerDisabledReason::Requirements { .. }
                        | codex_config::McpServerDisabledReason::ExplicitAndRequirements { .. }
                )
            )
        }) {
            return Err(FunctionCallError::RespondToModel(
                "native coordination is denied by managed MCP requirements".into(),
            ));
        }
    }
    // Select one complete policy owner; never mix native fields with legacy denial.
    let native = (approval.namespace == "codex_tui")
        .then_some(config.thread_coordination.as_ref())
        .flatten();
    let native_policy_owner =
        approval.namespace == "codex_tui" && (native.is_some() || server_config.is_none());
    let (enabled, configured_mode) = if let Some(native) = native {
        let policy = native.tools.get(tool);
        (
            native.enabled
                && native
                    .enabled_tools
                    .as_ref()
                    .is_none_or(|tools| tools.contains(tool))
                && native
                    .disabled_tools
                    .as_ref()
                    .is_none_or(|tools| !tools.contains(tool))
                && policy.is_none_or(|policy| policy.enabled != Some(false)),
            policy.and_then(|policy| policy.approval_mode),
        )
    } else if let Some(server) = server_config {
        (
            server.enabled
                && server
                    .enabled_tools
                    .as_ref()
                    .is_none_or(|tools| tools.contains(tool))
                && server
                    .disabled_tools
                    .as_ref()
                    .is_none_or(|tools| !tools.contains(tool)),
            tool_config
                .and_then(|policy| policy.approval_mode)
                .or(server.default_tools_approval_mode),
        )
    } else {
        (true, None)
    };
    if !enabled {
        return Err(FunctionCallError::RespondToModel(
            "tool is disabled by policy".into(),
        ));
    }
    // Preserve the former host's per-tool defaults: delegation prompts;
    // the remaining capabilities approve unless explicit policy overrides them.
    let mode = configured_mode.unwrap_or(match approval.default_mode {
        ToolApprovalMode::Prompt => AppToolApproval::Prompt,
        ToolApprovalMode::Approve => AppToolApproval::Approve,
    });
    let policy = McpToolApprovalPolicy::for_server(mode);
    // Native and user-configured extension policy is host-owned, not a scoped MCP peer.
    let server_key = McpServerKey::unscoped(&approval.namespace);
    let invocation = McpInvocation {
        server: approval.namespace,
        environment_id: None,
        tool: tool.clone(),
        arguments: Some(arguments),
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(rmcp::model::ToolAnnotations::new().read_only(approval.read_only)),
        connector_id: None,
        link_id: None,
        connector_name: None,
        connector_description: None,
        connected_account_email: None,
        plugin_id: None,
        tool_title: None,
        tool_description: Some(approval.description),
        mcp_app_resource_uri: None,
        mcp_app_ui: None,
        codex_apps_meta: None,
        openai_file_input_optional_fields: None,
    };
    let _metadata = call.session.register_mcp_tool_approval_metadata(
        &call.call_id,
        &server_key,
        &invocation,
        metadata.clone(),
        Arc::clone(&call.step_context),
        &call.cancellation_token,
        /*environment_permissions*/ None,
    );
    let mcp_config = config
        .to_mcp_config(&call.session.services.plugins_manager)
        .await;
    if mcp_config.client.requires_read_only_mcp_tools && !approval.read_only {
        return Err(FunctionCallError::RespondToModel(
            "tool is disabled by read-only policy".into(),
        ));
    }
    let permission_profile = mcp_config
        .permission_profile_for_server(&server_key)
        .filter(|_| !native_policy_owner)
        .unwrap_or(&mcp_config.permission_profile);
    // Native coordination is controller-owned; a legacy policy keeps its configured owner.
    let environment_id = server_config
        .filter(|_| !native_policy_owner)
        .map_or(codex_exec_server::LOCAL_ENVIRONMENT_ID, |server| {
            server.environment_id.as_str()
        });
    let decision = Box::pin(maybe_request_mcp_tool_approval(
        &call.session,
        &call.step_context,
        &call.cancellation_token,
        &call.call_id,
        &invocation,
        &call.tool_name,
        &metadata,
        &mcp_config,
        codex_mcp::McpPermissionSource::Thread {
            profile: permission_profile,
            host_cwd: &mcp_config.host_cwd,
        },
        environment_id,
        &server_key,
        policy,
    ))
    .await;
    match decision {
        None => Ok(()),
        Some(McpToolApproval {
            decision:
                decision @ (ReviewDecision::Approved
                | ReviewDecision::ApprovedForSession
                | ReviewDecision::ApprovedMcpPolicyAmendment
                | ReviewDecision::ApprovedExecpolicyAmendment { .. }
                | ReviewDecision::NetworkPolicyAmendment { .. }),
            ..
        }) => {
            let decision = if native_policy_owner
                && matches!(decision, ReviewDecision::ApprovedMcpPolicyAmendment)
            {
                // Explicit "always approve" consent updates native policy, not
                // an MCP server table with no transport.
                let result = crate::config::edit::ConfigEditsBuilder::for_config(config)
                    .with_edits([crate::config::edit::ConfigEdit::SetPath {
                        segments: vec![
                            "tools".into(),
                            "thread_coordination".into(),
                            "tools".into(),
                            tool.clone(),
                            "approval_mode".into(),
                        ],
                        value: toml_edit::value("approve"),
                    }])
                    .apply()
                    .await;
                if let Err(err) = result {
                    tracing::warn!(%err, "failed to persist native tool approval; remembering for this session");
                } else {
                    Box::pin(call.session.reload_user_config_layer()).await;
                }
                ReviewDecision::ApprovedForSession
            } else {
                decision
            };
            Box::pin(apply_mcp_tool_approval_decision(
                &call.session,
                &call.turn,
                &decision,
                session_mcp_tool_approval_key(
                    &server_key,
                    &invocation,
                    Some(&metadata),
                    policy.mode,
                ),
                persistent_mcp_tool_approval_key(&invocation, Some(&metadata), policy.mode),
            ))
            .await;
            Ok(())
        }
        Some(McpToolApproval {
            decision: ReviewDecision::Denied { rejection },
            ..
        }) => Err(FunctionCallError::RespondToModel(rejection)),
        Some(McpToolApproval {
            decision: ReviewDecision::TimedOut | ReviewDecision::Abort,
            ..
        }) => Err(FunctionCallError::RespondToModel(
            "tool approval was not granted".into(),
        )),
    }
}
