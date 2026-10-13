use super::*;

#[derive(Clone)]
pub(super) enum PermissionAuthority {
    Configured {
        profile: RuntimePermissionProfileOverride,
        approval_policy: AskForApproval,
        approvals_reviewer: ApprovalsReviewer,
    },
    Selected(PermissionProfileSelection),
}

impl PermissionAuthority {
    pub(super) fn capture(config: &Config, selected: Option<PermissionProfileSelection>) -> Self {
        match selected {
            Some(selected) => Self::Selected(selected),
            None => Self::Configured {
                profile: RuntimePermissionProfileOverride::from_config(config),
                approval_policy: config.permissions.approval_policy.value().into(),
                approvals_reviewer: config.approvals_reviewer,
            },
        }
    }

    pub(super) fn selection(&self) -> Option<&PermissionProfileSelection> {
        match self {
            Self::Selected(selection) => Some(selection),
            Self::Configured { .. } => None,
        }
    }

    pub(super) fn matches(
        &self,
        config: &Config,
        pending: Option<&PermissionProfileSelection>,
    ) -> bool {
        match (self, pending) {
            (Self::Selected(expected), Some(actual)) => {
                expected.profile_id == actual.profile_id
                    && expected.approval_policy == actual.approval_policy
                    && expected.approvals_reviewer == actual.approvals_reviewer
            }
            // Confirmation retires the request without changing its effective authority.
            (Self::Selected(expected), None) => {
                config
                    .permissions
                    .active_permission_profile()
                    .is_some_and(|active| active.id == expected.profile_id)
                    && expected.approval_policy.is_none_or(|policy| {
                        config.permissions.approval_policy.value() == policy.to_core()
                    })
                    && expected
                        .approvals_reviewer
                        .is_none_or(|reviewer| config.approvals_reviewer == reviewer)
            }
            (
                Self::Configured {
                    profile,
                    approval_policy,
                    approvals_reviewer,
                },
                None,
            ) => {
                profile.matches_config(config)
                    && config.permissions.approval_policy.value() == approval_policy.to_core()
                    && config.approvals_reviewer == *approvals_reviewer
            }
            (Self::Configured { .. }, Some(_)) => false,
        }
    }
}
