use crate::ModelProviderAdapter;
use crate::ModelProviderCapabilities;
use crate::ModelProviderInfo;
use crate::RemoteCompactionSupport;

pub const OPENROUTER_PROVIDER_ID: &str = "openrouter";

pub(crate) fn provider() -> ModelProviderInfo {
    ModelProviderInfo {
        adapter: ModelProviderAdapter::OpenRouter,
        name: "OpenRouter".into(),
        base_url: Some("https://openrouter.ai/api/v1".into()),
        request_max_retries: Some(4),
        stream_max_retries: Some(12),
        stream_idle_timeout_ms: Some(1_800_000),
        capabilities: Some(ModelProviderCapabilities {
            external_web_access: Some(false),
            remote_compaction: Some(RemoteCompactionSupport::Unsupported),
            cyber_access_programs: None,
        }),
        ..Default::default()
    }
}

pub(crate) fn validate(provider: &ModelProviderInfo) -> Result<(), String> {
    if provider.env_key.is_some()
        || provider.experimental_bearer_token.is_some()
        || provider.auth.is_some()
        || provider.gateway_oauth.is_some()
        || provider.model_catalog_url.is_some()
        || provider.requires_openai_auth
        || provider.supports_websockets
    {
        return Err("OpenRouter requires key_files authentication, saved models and HTTP streaming; env_key, experimental_bearer_token, auth, gateway_oauth, model_catalog_url, requires_openai_auth and websockets are not supported".into());
    }
    Ok(())
}
