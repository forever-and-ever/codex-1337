use super::*;
use crate::environment_selection::TurnEnvironment;
use codex_mcp::ElicitationApprovalAuthority;
use codex_mcp::IssuingElicitationReviewer;

impl IssuingElicitationReviewer for GuardianMcpElicitationReviewer {
    fn approval_authority(
        &self,
        request: ElicitationReviewRequest,
    ) -> futures::future::BoxFuture<'static, Option<ElicitationApprovalAuthority>> {
        let session = self.session.clone();
        Box::pin(async move {
            let session = session.upgrade()?;
            let (turn, _) = session.active_turn_context_and_cancellation_token().await?;
            let (context, config) = issuing_context(&session, &request, &turn)?;
            Some(project_authority(&context, &config, &request.server_name))
        })
    }
}

fn invocation_call_id(request: &ElicitationReviewRequest) -> Option<&str> {
    request
        .elicitation
        .meta()
        .and_then(|meta| match request.server_name.as_str() {
            CODEX_APPS_MCP_SERVER_NAME => meta.get(MCP_TOOL_CODEX_APPS_META_KEY)?.get("call_id"),
            _ => meta.get("callId"),
        })
        .and_then(Value::as_str)
}

fn issuing_context(
    session: &Session,
    request: &ElicitationReviewRequest,
    turn: &Arc<TurnContext>,
) -> Option<(
    crate::guardian::GuardianReviewContext,
    Arc<codex_mcp::McpConfig>,
)> {
    if let Some(invocation) = invocation_call_id(request)
        .and_then(|call_id| session.mcp_tool_approval_metadata(&request.server_name, call_id))
    {
        return Some((
            crate::guardian::GuardianReviewContext::from(&invocation.step_context),
            Arc::clone(invocation.step_context.mcp.config()),
        ));
    }
    // Server-initiated requests have no issuing invocation. They belong to the
    // active turn, including explicit live updates, not saved future-turn settings.
    let settings = turn.next_step_settings.load_full();
    Some((
        crate::guardian::GuardianReviewContext::from_resolved_settings(
            Arc::clone(turn),
            &settings,
            &turn.initial_environments,
        ),
        session.services.mcp_runtime.current_config()?,
    ))
}

fn project_authority(
    context: &crate::guardian::GuardianReviewContext,
    config: &codex_mcp::McpConfig,
    server_name: &str,
) -> ElicitationApprovalAuthority {
    let thread_profile = context
        .turn()
        .config
        .permissions
        .effective_permission_profile();
    let environment_profile = config
        .mcp_server_catalog
        .server(server_name)
        .and_then(|server| {
            context
                .environments()
                .turn_environments()
                .find(|environment| {
                    environment.selection.environment_id == server.config().environment_id
                })
        })
        .map(TurnEnvironment::permission_profile_with_workspace_roots);
    let permission_profile = config
        .server_permission_profile(server_name, &thread_profile, |_| {
            environment_profile.as_ref()
        })
        .cloned();
    ElicitationApprovalAuthority {
        approval_policy: context.approval_policy,
        permission_profile,
    }
}

pub(super) async fn review_guardian_mcp_elicitation(
    session: Arc<Session>,
    request: ElicitationReviewRequest,
) -> anyhow::Result<Option<ElicitationResponse>> {
    let Some((turn_context, cancellation_token)) =
        session.active_turn_context_and_cancellation_token().await
    else {
        return Ok(None);
    };

    let Some((mut review_context, mcp_config)) = issuing_context(&session, &request, &turn_context)
    else {
        return Ok(None);
    };
    let authority = project_authority(&review_context, &mcp_config, &request.server_name);
    let Some(permission_profile) = authority.permission_profile.as_ref() else {
        return Ok(Some(mcp_elicitation_decline_without_message()));
    };
    let model_info = Arc::clone(&review_context.model_info);
    let turn_context = Arc::clone(review_context.turn());

    // User approval skips ordinary CUA checks, not separate sensitive requests.
    let user_cua_execution = review_context.approvals_reviewer == ApprovalsReviewer::User
        && is_node_repl_backed_server(&request.server_name)
        && request.elicitation.meta().is_some_and(|meta| {
            metadata_str(meta, MCP_ELICITATION_TOOL_NAME_KEY) == Some("js")
                && metadata_str(meta, MCP_ELICITATION_CONNECTOR_ID_KEY) == Some("node_repl")
                && metadata_str(meta, MCP_ELICITATION_APPROVAL_KIND_KEY)
                    == Some(MCP_ELICITATION_APPROVAL_KIND_MCP_TOOL_CALL)
                && meta.get(MCP_ELICITATION_SENSITIVE_ACTION_KEY) != Some(&Value::Bool(true))
                && meta.get("codex_requires_user_input") != Some(&Value::Bool(true))
        });

    // Full Access skips inference, not the active-turn and cancellation checks.
    if (user_cua_execution
        || codex_mcp::mcp_permission_prompt_is_auto_approved(
            authority.approval_policy,
            permission_profile,
            codex_mcp::McpPermissionPromptAutoApproveContext::default(),
        ))
        && matches!(
            &request.elicitation,
            Elicitation::Mcp(rmcp::model::ElicitRequestParams::FormElicitationParams {
                requested_schema, ..
            }) if requested_schema.properties.is_empty()
        )
    {
        let decision = if cancellation_token.is_cancelled() {
            ReviewDecision::Abort
        } else {
            ReviewDecision::Approved
        };
        return Ok(Some(mcp_elicitation_response_from_guardian_decision(
            decision,
            &model_info,
        )));
    }

    // The invocation identifies the tool event, but a nested elicitation can
    // review a different action and connector than the enclosing JavaScript.
    let call_id = invocation_call_id(&request);
    let approval_context = call_id
        .and_then(|call_id| session.mcp_tool_approval_metadata(&request.server_name, call_id));
    let (originating_call_id, guardian_scope) = if let Some(call_id) = call_id
        && let Some(McpToolApprovalContext {
            invocation: Some(invocation),
            metadata,
            ..
        }) = approval_context.as_ref()
        && invocation.server == request.server_name
        && is_node_repl_backed_connector(&invocation.server, metadata.connector_id.as_deref())
    {
        (Some(call_id), GuardianScope::ComputerUse)
    } else {
        (None, GuardianScope::for_mcp_server(&request.server_name))
    };

    let require_synchronous_review = matches!(
        request
            .elicitation
            .meta()
            .and_then(|meta| meta.get(MCP_ELICITATION_SENSITIVE_ACTION_KEY)),
        Some(Value::Bool(true))
    );

    let strict_auto_review = matches!(
        request
            .elicitation
            .meta()
            .and_then(|meta| meta.get(MCP_ELICITATION_STRICT_AUTO_REVIEW_KEY)),
        Some(Value::Bool(true))
    );
    let mut guardian_request: crate::guardian::ReviewAction = if strict_auto_review {
        let connector_id = elicitation_connector_id(&request.elicitation);
        // A live Browser invocation can review a nested action with its own identity.
        // Other hosted connectors must still review their registered outer invocation.
        let review_outer_invocation =
            request.server_name == CODEX_APPS_MCP_SERVER_NAME && originating_call_id.is_none();
        let trusted_guardian_request = if review_outer_invocation {
            let Some(call_id) = call_id else {
                return Ok(None);
            };
            let Some(McpToolApprovalContext {
                invocation: Some(invocation),
                metadata,
                ..
            }) = approval_context.as_ref()
            else {
                return Ok(None);
            };
            if invocation.server != request.server_name
                || connector_id != metadata.connector_id.as_deref()
                || request
                    .elicitation
                    .meta()
                    .and_then(|meta| metadata_str(meta, MCP_ELICITATION_TOOL_NAME_KEY))
                    != Some(invocation.tool.as_str())
            {
                return Ok(None);
            }
            Some(
                crate::mcp_tool_call::build_guardian_mcp_tool_review_request(
                    call_id,
                    invocation,
                    Some(metadata),
                ),
            )
        } else {
            None
        };
        if !turn_context
            .config
            .features
            .enabled(Feature::GuardianApproval)
            || turn_context
                .config
                .config_layer_stack
                .requirements()
                .approvals_reviewer
                .can_set(&ApprovalsReviewer::AutoReview)
                .is_err()
            || crate::connectors::mcp_approvals_reviewer_from_layers(
                &turn_context.config.config_layer_stack,
                ApprovalsReviewer::AutoReview,
                Some(model_info.slug.as_str()),
                request.server_name.as_str(),
                connector_id,
                /*link_id*/ None,
            ) != ApprovalsReviewer::AutoReview
            || request
                .elicitation
                .meta()
                .is_some_and(|meta| meta.contains_key(MCP_ELICITATION_PERSIST_KEY))
        {
            return Ok(None);
        }

        let GuardianElicitationReview::ApprovalRequest(guardian_request) =
            guardian_elicitation_review_request(&request, originating_call_id)
        else {
            return Ok(None);
        };
        trusted_guardian_request.unwrap_or(*guardian_request).into()
    } else {
        let approval_policy = authority.approval_policy;
        match approval_policy {
            AskForApproval::Never => {
                if codex_mcp::mcp_permission_prompt_is_auto_approved(
                    approval_policy,
                    permission_profile,
                    codex_mcp::McpPermissionPromptAutoApproveContext::default(),
                ) && matches!(
                    &request.elicitation,
                    Elicitation::Mcp(
                        rmcp::model::ElicitRequestParams::FormElicitationParams {
                            requested_schema,
                            ..
                        }
                    ) if requested_schema.properties.is_empty()
                ) {
                    return Ok(Some(ElicitationResponse {
                        action: ElicitationAction::Accept,
                        content: Some(serde_json::json!({})),
                        meta: None,
                    }));
                }
                return Ok(Some(mcp_elicitation_decline_without_message()));
            }
            AskForApproval::Granular(config) if !config.allows_mcp_elicitations() => {
                return Ok(Some(mcp_elicitation_decline_without_message()));
            }
            AskForApproval::OnRequest
            | AskForApproval::UnlessTrusted
            | AskForApproval::Granular(_) => {}
        }

        let approvals_reviewer = crate::connectors::mcp_approvals_reviewer_from_layers(
            &turn_context.config.config_layer_stack,
            review_context.approvals_reviewer,
            Some(model_info.slug.as_str()),
            request.server_name.as_str(),
            elicitation_connector_id(&request.elicitation),
            /*link_id*/ None,
        );
        review_context.approval_policy = approval_policy;
        review_context.approvals_reviewer = approvals_reviewer;
        match guardian_elicitation_review_request(&request, originating_call_id) {
            GuardianElicitationReview::NotRequested => return Ok(None),
            GuardianElicitationReview::Decline(reason) => {
                let Elicitation::Mcp(elicitation) = &request.elicitation else {
                    return Ok(None);
                };
                crate::guardian::ReviewAction {
                    action: serde_json::to_value(elicitation)
                        .map(|elicitation| {
                            serde_json::json!({
                                "tool": "mcp_elicitation",
                                "server": request.server_name,
                                "request": elicitation,
                            })
                        })
                        .map_err(|error| error.to_string()),
                    category: GuardianScope::for_mcp_server(&request.server_name),
                    request: Err(reason.to_owned()),
                }
            }
            GuardianElicitationReview::ApprovalRequest(guardian_request) => {
                (*guardian_request).into()
            }
        }
    };
    guardian_request.category = guardian_scope;
    let declined_reason = guardian_request.request.as_ref().err().cloned();
    let decision = crate::guardian::decide_approval(
        session,
        review_context,
        crate::guardian::new_guardian_review_id(),
        guardian_request,
        crate::tools::sandboxing::ApprovalRequestReasons {
            approval: None,
            retry: None,
        },
        crate::guardian::GuardianReviewOptions {
            require_guardian: strict_auto_review,
            plugin_attribution_override: None,
            approval_request_source: codex_analytics::GuardianApprovalRequestSource::MainTurn,
            external_cancel: Some(cancellation_token),
            require_synchronous_review,
        },
    )
    .await;
    Ok(decision.map(|decision| {
        if let Some(reason) = declined_reason {
            warn!(server_name = %request.server_name,
                request_id = %mcp_elicitation_request_id(&request.request_id), reason,
                "declining Guardian MCP elicitation before review");
            return mcp_elicitation_decline_without_message();
        }
        mcp_elicitation_response_from_guardian_decision(decision, &model_info)
    }))
}
