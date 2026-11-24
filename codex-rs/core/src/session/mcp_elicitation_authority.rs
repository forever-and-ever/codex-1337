use super::*;
use crate::session::turn_context::TurnEnvironment;
use codex_mcp::ElicitationApprovalAuthority;
use codex_mcp::IssuingElicitationReviewer;

impl IssuingElicitationReviewer for GuardianMcpElicitationReviewer {
    fn approval_authority(
        self: Arc<Self>,
        request: ElicitationReviewRequest,
    ) -> BoxFuture<'static, anyhow::Result<Option<codex_mcp::PreparedElicitationReview>>> {
        let session = self.session.clone();
        Box::pin(async move {
            let session = session
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("MCP elicitation session is unavailable"))?;
            // TELOS(idle-mcp-human-consent): no turn means no automated review authority.
            let Some(context) = issuing_context(&session, &request).await? else {
                return Ok(Some(codex_mcp::PreparedElicitationReview::Human));
            };
            anyhow::ensure!(
                !context.cancellation.is_cancelled(),
                "MCP elicitation issuing turn was cancelled"
            );
            let authority =
                project_authority(&context.review, &context.config, &request.server_key);
            Ok(Some(codex_mcp::PreparedElicitationReview::Issued {
                authority,
                review: Box::pin(review_bound_elicitation(session, request, context)),
            }))
        })
    }
}

fn invocation_call_id(request: &ElicitationReviewRequest) -> anyhow::Result<Option<&str>> {
    let Some(meta) = request.elicitation.meta() else {
        return Ok(None);
    };
    let value = match request.server_name.as_str() {
        CODEX_APPS_MCP_SERVER_NAME if request.server_key.scoped_environment_id().is_none() => {
            match meta.get(MCP_TOOL_CODEX_APPS_META_KEY) {
                Some(value) => value
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("invalid MCP invocation metadata"))?
                    .get("call_id"),
                None => None,
            }
        }
        _ => meta.get("callId"),
    };
    value
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid MCP invocation call ID"))
        })
        .transpose()
}

struct ElicitationReviewContext {
    review: crate::guardian::GuardianReviewContext,
    config: Arc<McpConfig>,
    invocation: Option<McpToolApprovalContext>,
    cancellation: CancellationToken,
}

async fn issuing_context(
    session: &Session,
    request: &ElicitationReviewRequest,
) -> anyhow::Result<Option<ElicitationReviewContext>> {
    // Pin the invocation before the first await: the registry is weak and its
    // running-tool guard may be dropped while this request is still waiting.
    let invocation = invocation_call_id(request)?
        .map(|call_id| {
            session
                .mcp_tool_approval_metadata(&request.server_key, call_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("MCP elicitation invocation {call_id} is unavailable")
                })
        })
        .transpose()?;
    let Some((turn, active_cancellation)) =
        session.active_turn_context_and_cancellation_token().await
    else {
        anyhow::ensure!(invocation.is_none(), "MCP elicitation has no active turn");
        return Ok(None);
    };
    let (review, config, cancellation) = if let Some(invocation) = invocation.as_ref() {
        (
            crate::guardian::GuardianReviewContext::from(&invocation.step_context),
            Arc::clone(invocation.step_context.mcp.config()),
            invocation.cancellation_token.child_token(),
        )
    } else {
        // Only requests without an invocation identity are server-initiated.
        // They belong to active-turn settings, never saved future-turn defaults.
        let settings = turn.next_step_settings.load_full();
        (
            crate::guardian::GuardianReviewContext::from_resolved_settings(
                Arc::clone(&turn),
                &settings,
                &turn.initial_environments,
            ),
            session
                .services
                .mcp_runtime
                .current_config()
                .ok_or_else(|| anyhow::anyhow!("MCP elicitation runtime is unavailable"))?,
            active_cancellation,
        )
    };
    Ok(Some(ElicitationReviewContext {
        review,
        config,
        invocation,
        cancellation,
    }))
}

fn project_authority(
    context: &crate::guardian::GuardianReviewContext,
    config: &McpConfig,
    server_key: &McpServerKey,
) -> ElicitationApprovalAuthority {
    let thread_profile = context
        .turn()
        .config
        .permissions
        .effective_permission_profile();
    let environment_profile = config
        .mcp_server_catalog
        .server(server_key)
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
        .server_permission_profile(server_key, &thread_profile, |_| {
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
    let Some(context) = issuing_context(&session, &request).await? else {
        return Ok(None);
    };
    review_bound_elicitation(session, request, context).await
}

async fn review_bound_elicitation(
    session: Arc<Session>,
    request: ElicitationReviewRequest,
    context: ElicitationReviewContext,
) -> anyhow::Result<Option<ElicitationResponse>> {
    let ElicitationReviewContext {
        review: mut review_context,
        config: mcp_config,
        invocation: approval_context,
        cancellation: cancellation_token,
    } = context;
    let authority = project_authority(&review_context, &mcp_config, &request.server_key);
    let Some(permission_profile) = authority.permission_profile.as_ref() else {
        return Ok(Some(mcp_elicitation_decline_without_message()));
    };
    let model_info = Arc::clone(&review_context.model_info);
    if cancellation_token.is_cancelled() {
        return Ok(Some(mcp_elicitation_response_from_guardian_decision(
            ReviewDecision::Abort,
            &model_info,
        )));
    }
    let turn_context = Arc::clone(review_context.turn());

    let is_js_preflight = request.elicitation.meta().is_some_and(|meta| {
        metadata_str(meta, MCP_ELICITATION_TOOL_NAME_KEY) == Some("js")
            && metadata_str(meta, MCP_ELICITATION_CONNECTOR_ID_KEY) == Some("node_repl")
    });
    // User approval skips ordinary CUA checks, not separate sensitive requests.
    let user_cua_execution = review_context.approvals_reviewer == ApprovalsReviewer::User
        && is_node_repl_backed_server(&request.server_name)
        && is_js_preflight
        && request.elicitation.meta().is_some_and(|meta| {
            metadata_str(meta, MCP_ELICITATION_APPROVAL_KIND_KEY)
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
    let call_id = invocation_call_id(&request)?;
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
    let connector_id = elicitation_connector_id(&request.elicitation);
    let review_outer_invocation =
        request.server_name == CODEX_APPS_MCP_SERVER_NAME && originating_call_id.is_none();
    let verified_invocation = if review_outer_invocation
        && let Some(call_id) = call_id
        && let Some(McpToolApprovalContext {
            invocation: Some(invocation),
            metadata,
            ..
        }) = approval_context.as_ref()
        && invocation.server == request.server_name
        && metadata.connector_id.as_deref() == connector_id
        && request
            .elicitation
            .meta()
            .and_then(|meta| metadata_str(meta, MCP_ELICITATION_TOOL_NAME_KEY))
            == Some(invocation.tool.as_str())
    {
        Some((call_id, invocation, metadata))
    } else {
        None
    };
    let link_id = verified_invocation.and_then(|(_, _, meta)| meta.link_id.as_deref());
    let tool = verified_invocation
        .map(|(_, invocation, meta)| (invocation.tool.as_str(), meta.tool_title.as_deref()));
    let requirements = turn_context.config.config_layer_stack.requirements();
    let reviewer_for = |layers, default_reviewer| {
        crate::connectors::mcp_approvals_reviewer_from_layers(
            layers,
            requirements,
            default_reviewer,
            Some(model_info.slug.as_str()),
            request.server_name.as_str(),
            connector_id,
            link_id,
            tool,
        )
    };

    let mut guardian_request: crate::guardian::ReviewAction = if strict_auto_review {
        let trusted_guardian_request = if review_outer_invocation {
            let Some((call_id, invocation, metadata)) = verified_invocation else {
                return Ok(None);
            };
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
            || requirements
                .approvals_reviewer
                .can_set(&ApprovalsReviewer::AutoReview)
                .is_err()
            || reviewer_for(
                &mcp_config.config_layer_stack,
                ApprovalsReviewer::AutoReview,
            ) != ApprovalsReviewer::AutoReview
            || request
                .elicitation
                .meta()
                .is_some_and(|meta| meta.contains_key(MCP_ELICITATION_PERSIST_KEY))
        {
            return Ok(None);
        }

        let GuardianElicitationReview::ApprovalRequest {
            request: guardian_request,
            tool_call_id,
        } = guardian_elicitation_review_request(&request, originating_call_id)
        else {
            return Ok(None);
        };
        match trusted_guardian_request {
            Some(request) => request.into(),
            None => crate::guardian::ReviewAction {
                tool_call_id,
                ..(*guardian_request).into()
            },
        }
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

        // Reviewer choice and managed requirements follow the issuing call, not a later
        // transport refresh. Verified invocations also carry the captured app tool and link.
        review_context.approval_policy = approval_policy;
        review_context.approvals_reviewer = reviewer_for(
            &mcp_config.config_layer_stack,
            review_context.approvals_reviewer,
        );
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
                    tool_call_id: None,
                    request: Err(reason.to_owned()),
                }
            }
            GuardianElicitationReview::ApprovalRequest {
                request: guardian_request,
                tool_call_id,
            } => crate::guardian::ReviewAction {
                tool_call_id,
                ..(*guardian_request).into()
            },
        }
    };
    guardian_request.category = guardian_scope;
    if !is_js_preflight
        && turn_context
            .config
            .features
            .enabled(Feature::GuardianNestedApprovalContext)
        && let Some(call_id) = originating_call_id
        && matches!(
            &guardian_request.request,
            Ok(crate::guardian::GuardianApprovalRequest::McpToolCall { .. })
        )
    {
        review_context.enclosing_invocation_id = Some(call_id.to_owned());
    }
    if let Ok(crate::guardian::GuardianApprovalRequest::McpToolCall {
        prompt_permissions, ..
    }) = &mut guardian_request.request
        && let Some(permissions) =
            approval_context.and_then(|context| context.environment_permissions)
    {
        *prompt_permissions = Some(permissions);
    }
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
                request_id = %request.request_id, reason,
                "declining Guardian MCP elicitation before review");
            return mcp_elicitation_decline_without_message();
        }
        mcp_elicitation_response_from_guardian_decision(decision, &model_info)
    }))
}
