use super::APPROVAL_KIND_KEY;
use super::APPROVALS_REVIEWER_KEY;
use super::AskForApproval;
use super::Elicitation;
use super::ElicitationAction;
use super::ElicitationLifecycle;
use super::ElicitationRequestRouter;
use super::ElicitationResponse;
use super::McpConfig;
use super::McpPermissionPromptAutoApproveContext;
use super::Result;
use super::STRICT_AUTO_REVIEW_KEY;
use super::Value;
use super::can_auto_accept_elicitation;
use super::elicitation_is_rejected_by_policy;
use super::mcp_permission_prompt_is_auto_approved;
use super::strict_auto_review_decline;
use crate::McpServerKey;
use crate::PreparedElicitationReview;

pub(super) async fn resolve(
    prepared: PreparedElicitationReview,
    config: &McpConfig,
    server_key: &McpServerKey,
    elicitation: &Elicitation,
    router: &ElicitationRequestRouter,
    has_reviewer: bool,
    lifecycle: Option<&ElicitationLifecycle>,
) -> Result<Option<ElicitationResponse>> {
    let (authority, review) = match prepared {
        // TELOS(idle-mcp-human-consent): connection policy may reject, never approve.
        PreparedElicitationReview::Human => (
            crate::ElicitationApprovalAuthority {
                approval_policy: config.approval_policy.value(),
                permission_profile: config.permission_profile_for_server(server_key).cloned(),
            },
            None,
        ),
        PreparedElicitationReview::Issued { authority, review } => (authority, Some(review)),
    };
    let approval_policy = authority.approval_policy;
    let Some(permission_profile) = authority.permission_profile.as_ref() else {
        return Ok(Some(decline()));
    };
    match elicitation
        .meta()
        .and_then(|meta| meta.get(STRICT_AUTO_REVIEW_KEY))
    {
        Some(Value::Bool(true)) => {
            if matches!(approval_policy, AskForApproval::Granular(config) if !config.allows_mcp_elicitations())
                || !has_reviewer
            {
                return Ok(Some(strict_auto_review_decline()));
            }
            let Some(review) = review else {
                return Ok(Some(strict_auto_review_decline()));
            };
            let _active_elicitation = lifecycle.map(ElicitationLifecycle::start);
            return Ok(Some(match review.await {
                Ok(Some(response))
                    if (response.action == ElicitationAction::Accept
                        && response.content == Some(serde_json::json!({}))
                        || matches!(
                            response.action,
                            ElicitationAction::Decline | ElicitationAction::Cancel
                        ) && response.content.is_none())
                        && response
                            .meta
                            .as_ref()
                            .and_then(Value::as_object)
                            .and_then(|meta| meta.get(APPROVALS_REVIEWER_KEY))
                            .and_then(Value::as_str)
                            == Some("auto_review") =>
                {
                    response
                }
                Ok(Some(_) | None) | Err(_) => strict_auto_review_decline(),
            }));
        }
        None | Some(Value::Bool(false)) => {}
        Some(_) => return Ok(Some(strict_auto_review_decline())),
    }
    let permission_prompt_is_auto_approved = mcp_permission_prompt_is_auto_approved(
        approval_policy,
        permission_profile,
        McpPermissionPromptAutoApproveContext::default(),
    );
    if review.is_some()
        && permission_prompt_is_auto_approved
        && can_auto_accept_elicitation(elicitation)
    {
        return Ok(Some(ElicitationResponse {
            action: ElicitationAction::Accept,
            content: Some(serde_json::json!({})),
            meta: None,
        }));
    }
    let should_surface_form_in_full_access = router.full_access_form_input_enabled()
        && permission_prompt_is_auto_approved
        && !elicitation
            .meta()
            .is_some_and(|meta| meta.contains_key(APPROVAL_KIND_KEY))
        && match elicitation {
            Elicitation::Mcp(rmcp::model::ElicitRequestParams::FormElicitationParams {
                requested_schema,
                ..
            }) => !requested_schema.properties.is_empty(),
            Elicitation::OpenAiElicitationForm {
                requested_schema, ..
            } => requested_schema
                .get("properties")
                .and_then(Value::as_object)
                .is_some_and(|properties| !properties.is_empty()),
            Elicitation::Mcp(_)
            | Elicitation::OpenAiForm { .. }
            | Elicitation::UserVerification { .. } => false,
        };
    if !should_surface_form_in_full_access {
        if elicitation_is_rejected_by_policy(approval_policy) {
            return Ok(Some(decline()));
        }
        if let Some(review) = review {
            return review.await;
        }
    }
    Ok(None)
}

fn decline() -> ElicitationResponse {
    ElicitationResponse {
        action: ElicitationAction::Decline,
        content: None,
        meta: None,
    }
}
