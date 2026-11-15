//! Realtime admits the same frozen credential source as other model requests.
use codex_model_provider::ProviderAuthContext;
use codex_protocol::error::Result;

pub(super) fn capture(context: ProviderAuthContext) -> Result<ProviderAuthContext> {
    let credentials = &context.credentials;
    let source = credentials.capture_job_source()?;
    source.validate()?;
    Ok(ProviderAuthContext {
        credentials: source.credentials(),
        runtime_auth: context.runtime_auth,
    })
}
