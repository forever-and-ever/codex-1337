use super::Session;
use crate::CapturedProvider;
use crate::CodexThread;
use crate::ProviderCaptureError;

impl Session {
    pub(crate) async fn capture_provider(&self) -> Result<CapturedProvider, ProviderCaptureError> {
        let _settings = super::thread_settings::acquire_persistence_lock(self).await;
        self.services.model_client.capture_provider()
    }

    pub(crate) fn capture_turn_provider(
        &self,
        turn: &super::turn_context::TurnContext,
    ) -> Result<(), ProviderCaptureError> {
        let captured = self.services.model_client.capture_provider()?;
        captured.matches(turn.provider.info())?;
        turn.extension_data.insert(captured);
        Ok(())
    }
}

impl CodexThread {
    pub async fn capture_provider(&self) -> Result<CapturedProvider, ProviderCaptureError> {
        self.session.capture_provider().await
    }
}
