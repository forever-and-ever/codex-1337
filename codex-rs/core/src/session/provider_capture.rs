use super::Session;
use crate::CapturedProvider;
use crate::CodexThread;
use crate::ProviderCaptureError;

impl Session {
    async fn capture_provider(&self) -> Result<CapturedProvider, ProviderCaptureError> {
        let _settings = super::thread_settings::acquire_persistence_lock(self).await;
        self.services.model_client.capture_provider().await
    }
}

impl CodexThread {
    pub async fn capture_provider(&self) -> Result<CapturedProvider, ProviderCaptureError> {
        self.session.capture_provider().await
    }
}
