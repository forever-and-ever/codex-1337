use std::sync::Arc;

use anyhow::Result;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_rmcp_client::ElicitationResponse;
use futures::future::BoxFuture;

use crate::elicitation::ElicitationReviewRequest;
use crate::elicitation::ElicitationReviewer;

// A boundary projection, never a second persistent policy owner.
#[derive(Clone)]
pub struct ElicitationApprovalAuthority {
    pub approval_policy: AskForApproval,
    // Missing issued permissions deny the request; they never use published defaults.
    pub permission_profile: Option<PermissionProfile>,
}

pub enum PreparedElicitationReview {
    Human,
    Issued {
        authority: ElicitationApprovalAuthority,
        review: BoxFuture<'static, Result<Option<ElicitationResponse>>>,
    },
}

pub trait IssuingElicitationReviewer: ElicitationReviewer {
    // Only hosts without issuing-step authority delegate to connection-wide policy.
    fn approval_authority(
        self: Arc<Self>,
        _request: ElicitationReviewRequest,
    ) -> BoxFuture<'static, Result<Option<PreparedElicitationReview>>> {
        Box::pin(async { Ok(None) })
    }
}

pub(crate) async fn resolve(
    reviewer: Option<&crate::ElicitationReviewerHandle>,
    config: &crate::McpConfig,
    request: ElicitationReviewRequest,
) -> Result<PreparedElicitationReview> {
    if let Some(reviewer) = reviewer
        && let Some(prepared) = Arc::clone(reviewer)
            .approval_authority(request.clone())
            .await?
    {
        return Ok(prepared);
    }
    let authority = ElicitationApprovalAuthority {
        approval_policy: config.approval_policy.value(),
        permission_profile: config
            .permission_profile_for_server(&request.server_key)
            .cloned(),
    };
    let reviewer = reviewer.cloned();
    Ok(PreparedElicitationReview::Issued {
        authority,
        review: Box::pin(async move {
            match reviewer {
                Some(reviewer) => reviewer.review(request).await,
                None => Ok(None),
            }
        }),
    })
}
