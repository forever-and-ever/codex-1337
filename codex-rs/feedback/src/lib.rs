//! Inert remnant of the removed `/feedback` upload. TELOS(no-feedback-upload): upstream threads
//! these names through many files; keeping them here, inert, lets those files merge unchanged.
//! Nothing is captured, retained or uploaded. Process diagnostics live in `codex-flight-recorder`.

use codex_login::AuthEnvTelemetry;

/// Placeholder for upstream's feedback sink; carries nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct CodexFeedback;

impl CodexFeedback {
    pub fn new() -> Self {
        Self
    }
}

/// Request and auth fields upstream records as feedback tags; accepted and discarded.
pub struct FeedbackRequestTags<'a> {
    pub endpoint: &'a str,
    pub auth_header_attached: bool,
    pub auth_header_name: Option<&'a str>,
    pub auth_mode: Option<&'a str>,
    pub auth_retry_after_unauthorized: Option<bool>,
    pub auth_recovery_mode: Option<&'a str>,
    pub auth_recovery_phase: Option<&'a str>,
    pub auth_connection_reused: Option<bool>,
    pub auth_request_id: Option<&'a str>,
    pub auth_cf_ray: Option<&'a str>,
    pub auth_error: Option<&'a str>,
    pub auth_error_code: Option<&'a str>,
    pub auth_recovery_followup_success: Option<bool>,
    pub auth_recovery_followup_status: Option<u16>,
}

pub fn emit_feedback_request_tags(_tags: &FeedbackRequestTags<'_>) {}

pub fn emit_feedback_request_tags_with_auth_env(
    _tags: &FeedbackRequestTags<'_>,
    _auth_env: &AuthEnvTelemetry,
) {
}
