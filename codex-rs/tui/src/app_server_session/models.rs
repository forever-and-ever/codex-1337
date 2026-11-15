//! Fetch picker models without blocking the event loop and keep new-thread defaults in sync.

use super::AppServerSession;
use super::model_preset_from_api_model;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::model_catalog::ModelListing;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
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
            let response = request_handle
                .request_typed::<ModelListResponse>(ClientRequest::ModelList {
                    request_id: RequestId::String(format!("model-list-{request_id}")),
                    params: ModelListParams {
                        model_provider: Some(provider.clone()),
                        cursor: None,
                        limit: None,
                        include_hidden: Some(true),
                        thread_id: thread_id.map(|id| id.to_string()),
                    },
                })
                .await;
            let result = match response {
                Err(error) => Err(error.to_string()),
                Ok(response) if response.model_provider.as_deref() != Some(provider.as_str()) => {
                    Err("The server does not support selecting this provider.".into())
                }
                Ok(response) => Ok(ModelListing {
                    providers: response.providers,
                    models: response
                        .data
                        .into_iter()
                        .map(model_preset_from_api_model)
                        .collect(),
                }),
            };
            app_event_tx.send(AppEvent::ModelsLoaded { request_id, result });
        });
    }
}

pub(super) async fn list(
    request_handle: AppServerRequestHandle,
    request_id: RequestId,
    model_provider: Option<String>,
    thread_id: Option<codex_protocol::ThreadId>,
) -> Result<Vec<ModelPreset>, TypedRequestError> {
    let selected_provider = model_provider.clone();
    let response: ModelListResponse = match request_handle
        .request_typed(ClientRequest::ModelList {
            request_id,
            params: ModelListParams {
                cursor: None,
                limit: None,
                include_hidden: Some(true),
                model_provider,
                thread_id: thread_id.map(|id| id.to_string()),
            },
        })
        .await
    {
        Ok(response) => response,
        Err(error) => return Err(error),
    };
    if selected_provider
        .as_deref()
        .is_some_and(|provider| response.model_provider.as_deref() != Some(provider))
    {
        return Err(TypedRequestError::Deserialize {
            method: "model/list".to_owned(),
            source: <serde_json::Error as serde::de::Error>::custom(
                "the server did not honor the selected model provider",
            ),
        });
    }
    Ok(response
        .data
        .into_iter()
        .map(model_preset_from_api_model)
        .collect())
}
