//! Switching provider starts a new conversation; the current history is never reinterpreted.

use super::*;
use crate::model_catalog::ModelListing;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::ModelProviderOption;
use codex_app_server_protocol::RequestId;

#[derive(Clone, Debug)]
pub(crate) struct ProviderModelChoice {
    pub(crate) provider: ModelProviderOption,
    pub(crate) model: ModelPreset,
    pub(crate) effort: codex_protocol::openai_models::ReasoningEffort,
}

#[derive(Debug)]
pub(crate) enum ProviderModelEvent {
    Open(ModelProviderOption),
    Loaded {
        request_id: Uuid,
        provider: ModelProviderOption,
        result: Result<ModelListing, String>,
    },
    Start(Box<ProviderModelChoice>),
}

impl App {
    pub(super) async fn handle_provider_model_event(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        event: ProviderModelEvent,
    ) {
        match event {
            ProviderModelEvent::Open(provider)
                if provider.id == codex_model_provider_info::OPENROUTER_PROVIDER_ID =>
            {
                self.handle_openrouter_event(
                    server,
                    crate::openrouter::Event::Request(crate::openrouter::Request::Models {
                        menu: crate::openrouter::ModelMenu::New(provider),
                        selected: None,
                    }),
                );
            }
            ProviderModelEvent::Open(provider) => {
                let request_id = Uuid::new_v4();
                self.chat_widget
                    .show_provider_models_loading(request_id, &provider);
                let handle = server.request_handle();
                let events = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let result = tokio::time::timeout(Duration::from_secs(/*secs*/ 20), async {
                        let response = handle
                            .request_typed::<ModelListResponse>(ClientRequest::ModelList {
                                request_id: RequestId::String(format!(
                                    "provider-models-{request_id}"
                                )),
                                params: ModelListParams {
                                    model_provider: Some(provider.id.clone()),
                                    include_hidden: Some(false),
                                    ..Default::default()
                                },
                            })
                            .await
                            .map_err(|err| err.to_string())?;
                        // Old servers ignore the new selector. Never treat their default catalog
                        // as evidence that a different provider can run those models.
                        if response.model_provider.as_deref() != Some(provider.id.as_str()) {
                            return Err(
                                "The server does not support selecting this provider.".into()
                            );
                        }
                        Ok(ModelListing {
                            models: response
                                .data
                                .into_iter()
                                .map(crate::app_server_session::model_preset_from_api_model)
                                .collect(),
                            providers: response.providers,
                        })
                    })
                    .await
                    .unwrap_or_else(|_| Err("Fetching models timed out.".into()));
                    events.send(AppEvent::ProviderModel(ProviderModelEvent::Loaded {
                        request_id,
                        provider,
                        result,
                    }));
                });
            }
            ProviderModelEvent::Loaded {
                request_id,
                provider,
                result,
            } => {
                self.chat_widget
                    .show_provider_models(request_id, &provider, result);
            }
            ProviderModelEvent::Start(choice) => {
                if self.chat_widget.session_draft.is_some() {
                    self.select_draft_provider(server, *choice).await;
                    return;
                }
                self.start_fresh_session_with_provider(
                    tui,
                    server,
                    /*session_start_source*/ None,
                    /*initial_user_message*/ None,
                    /*new_thread_name*/ None,
                    Some(*choice),
                )
                .await;
            }
        }
    }
}
