use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use futures::future::BoxFuture;

use crate::elicitation::ElicitationReviewRequest;
use crate::elicitation::ElicitationReviewer;

// A boundary projection, never a second persistent policy owner.
#[derive(Clone)]
pub struct ElicitationApprovalAuthority {
    pub approval_policy: AskForApproval,
    pub permission_profile: Option<PermissionProfile>,
}

pub trait IssuingElicitationReviewer: ElicitationReviewer {
    fn approval_authority(
        &self,
        _request: ElicitationReviewRequest,
    ) -> BoxFuture<'static, Option<ElicitationApprovalAuthority>> {
        Box::pin(async { None })
    }
}

pub(crate) async fn resolve(
    reviewer: Option<&crate::ElicitationReviewerHandle>,
    config: &crate::McpConfig,
    request: ElicitationReviewRequest,
) -> ElicitationApprovalAuthority {
    let server_name = request.server_name.clone();
    if let Some(reviewer) = reviewer
        && let Some(authority) = reviewer.approval_authority(request).await
    {
        return authority;
    }
    ElicitationApprovalAuthority {
        approval_policy: config.approval_policy.value(),
        permission_profile: config.permission_profile_for_server(&server_name).cloned(),
    }
}
