//! Fetch picker models without blocking the event loop and keep new-thread defaults in sync.

use super::AppServerSession;
use super::model_preset_from_api_model;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::model_catalog::ModelListing;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::RequestId;
use codex_protocol::openai_models::ModelPreset;
use uuid::Uuid;

impl AppServerSession {
    pub(crate) fn set_available_models(&mut self, models: Vec<ModelPreset>) {
        if let Some(default) = models
            .iter()
            .find(|model| model.is_default)
            .or(models.first())
        {
            self.default_model = Some(default.model.clone());
        }
        self.available_models = models;
    }

    pub(crate) fn fetch_models(
        &self,
        request_id: Uuid,
        provider: String,
        thread_id: Option<codex_protocol::ThreadId>,
        app_event_tx: AppEventSender,
    ) {
        let request_handle = self.request_handle();
        tokio::spawn(async move {
            let result = request_handle
                .request_typed::<ModelListResponse>(ClientRequest::ModelList {
                    request_id: RequestId::String(format!("model-list-{request_id}")),
                    params: ModelListParams {
                        model_provider: Some(provider.clone()),
                        cursor: None,
                        limit: None,
                        include_hidden: Some(true),
                        // A conversation's picker lists what its own account offers.
                        thread_id: thread_id.map(|id| id.to_string()),
                    },
                })
                .await
                .map_err(|err| err.to_string())
                .and_then(|response| {
                    if response.model_provider.as_deref() != Some(provider.as_str()) {
                        return Err("The server does not support selecting this provider.".into());
                    }
                    Ok(ModelListing {
                        providers: response.providers,
                        models: response
                            .data
                            .into_iter()
                            .map(model_preset_from_api_model)
                            .collect(),
                    })
                });
            app_event_tx.send(AppEvent::ModelsLoaded { request_id, result });
        });
    }
}
