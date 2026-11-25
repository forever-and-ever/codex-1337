use super::AbsolutePathBuf;
use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::ClientRequest;
use super::Duration;
use super::Uuid;
use crate::openrouter::Event;
use crate::openrouter::Reply;
use crate::openrouter::Request;
use crate::openrouter::StatusFailure;
use crate::openrouter::StatusRefresh;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::OpenRouterKeyAddParams;
use codex_app_server_protocol::OpenRouterKeyAddResponse;
use codex_app_server_protocol::OpenRouterKeyRemoveParams;
use codex_app_server_protocol::OpenRouterKeyRemoveResponse;
use codex_app_server_protocol::OpenRouterKeyStatusParams;
use codex_app_server_protocol::OpenRouterKeyStatusResponse;
use codex_app_server_protocol::OpenRouterModelEndpointsParams;
use codex_app_server_protocol::OpenRouterModelEndpointsResponse;
use codex_app_server_protocol::OpenRouterModelForgetParams;
use codex_app_server_protocol::OpenRouterModelForgetResponse;
use codex_app_server_protocol::OpenRouterModelRouteParams;
use codex_app_server_protocol::OpenRouterModelRouteResponse;
use codex_app_server_protocol::OpenRouterModelSaveParams;
use codex_app_server_protocol::OpenRouterModelSaveResponse;
use codex_app_server_protocol::ThreadAccountListParams;
use codex_app_server_protocol::ThreadAccountListResponse;
use codex_model_provider_info::OPENROUTER_PROVIDER_ID;

impl App {
    pub(super) fn handle_openrouter_event(&mut self, server: &mut AppServerSession, event: Event) {
        match event {
            Event::SelectTransport {
                thread_id,
                model,
                transport,
            } => {
                let handle = server.request_handle();
                let request_id = server.next_request_id();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let ack = handle
                        .request_typed::<codex_app_server_protocol::ThreadSettingsUpdateResponse>(
                            ClientRequest::ThreadSettingsUpdate {
                                request_id,
                                params: codex_app_server_protocol::ThreadSettingsUpdateParams {
                                    thread_id: thread_id.to_string(),
                                    model,
                                    openrouter_transport: Some(transport),
                                    ..Default::default()
                                },
                            },
                        )
                        .await;
                    let result = match ack {
                        Ok(ack) if ack.openrouter_transport == Some(transport) => Ok(transport),
                        Ok(_) => {
                            Err("This server did not acknowledge OpenRouter batch mode.".into())
                        }
                        Err(error) => Err(error.to_string()),
                    };
                    tx.send(AppEvent::OpenRouter(Event::TransportSelected {
                        thread_id,
                        result,
                    }));
                });
            }
            Event::TransportSelected { thread_id, result } => {
                if self.current_displayed_thread_id() == Some(thread_id) {
                    match result {
                        Ok(codex_protocol::openrouter::OpenRouterTransport::Batch) => {
                            self.chat_widget.add_info_message(
                                "Future model steps use OpenRouter batch inference.".into(),
                                None,
                            );
                        }
                        Ok(codex_protocol::openrouter::OpenRouterTransport::Live) => {
                            self.chat_widget.add_info_message(
                                "Future model steps use live OpenRouter inference.".into(),
                                None,
                            );
                        }
                        Err(error) => self.chat_widget.add_error_message(error),
                    }
                }
            }
            Event::Error(error) => self.chat_widget.show_openrouter_error(error),
            Event::ModelSubmitted { menu, input } => match input.parse() {
                Ok(slug) => self
                    .handle_openrouter_event(server, Event::Request(Request::Save { menu, slug })),
                Err(error) => self
                    .chat_widget
                    .openrouter_model_prompt(menu, input, Some(error)),
            },
            Event::KeyNameSubmitted { thread_id, input } => match input.parse() {
                Ok(name) => {
                    self.handle_openrouter_event(server, Event::KeyPathPrompt { thread_id, name });
                }
                Err(error) => self.chat_widget.openrouter_key_name_prompt(
                    thread_id,
                    if input.starts_with("sk-or-") {
                        String::new()
                    } else {
                        input
                    },
                    Some(error),
                ),
            },
            Event::KeyFileSubmitted {
                thread_id,
                name,
                input,
            } => match AbsolutePathBuf::try_from(input.as_str()) {
                Ok(path) => self.handle_openrouter_event(
                    server,
                    Event::Request(Request::AddKey {
                        thread_id,
                        name,
                        path,
                    }),
                ),
                Err(error) => self.chat_widget.openrouter_key_path_prompt(
                    thread_id,
                    name,
                    if input.starts_with("sk-or-") {
                        String::new()
                    } else {
                        input
                    },
                    Some(error.to_string()),
                ),
            },
            Event::AddModel { menu } => {
                self.chat_widget
                    .openrouter_model_prompt(menu, String::new(), None);
            }
            Event::KeyNamePrompt { thread_id } => {
                self.chat_widget
                    .openrouter_key_name_prompt(thread_id, String::new(), None);
            }
            Event::KeyPathPrompt { thread_id, name } => self
                .chat_widget
                .openrouter_key_path_prompt(thread_id, name, String::new(), None),
            Event::RemoveKeyConfirm { thread_id, name } => self
                .chat_widget
                .openrouter_remove_key_confirm(thread_id, name),
            Event::Request(request) => {
                let id = Uuid::new_v4();
                let thread_id = self.current_displayed_thread_id();
                if !self.chat_widget.start_openrouter_request(id, &request) {
                    return;
                }
                let handle = server.request_handle();
                let request_id = server.next_request_id();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let result = tokio::time::timeout(Duration::from_secs(30), async {
                        match &request {
                            Request::Models { .. } => {
                                let response = handle
                                    .request_typed::<ModelListResponse>(ClientRequest::ModelList {
                                        request_id,
                                        params: ModelListParams {
                                            model_provider: Some(OPENROUTER_PROVIDER_ID.into()),
                                            include_hidden: Some(false),
                                            ..Default::default()
                                        },
                                    })
                                    .await?;
                                if response.model_provider.as_deref()
                                    != Some(OPENROUTER_PROVIDER_ID)
                                {
                                    anyhow::bail!(
                                        "The server does not support OpenRouter model selection"
                                    );
                                }
                                Ok(Reply::Models(crate::model_catalog::ModelListing {
                                    models: response
                                        .data
                                        .into_iter()
                                        .map(crate::app_server_session::model_preset_from_api_model)
                                        .collect(),
                                    providers: response.providers,
                                }))
                            }
                            Request::Save { slug, .. } => {
                                let response = handle
                                    .request_typed::<OpenRouterModelSaveResponse>(
                                        ClientRequest::OpenRouterModelSave {
                                            request_id,
                                            params: OpenRouterModelSaveParams {
                                                slug: slug.clone(),
                                            },
                                        },
                                    )
                                    .await?;
                                Ok(Reply::Saved(
                                    response.model.model.parse().map_err(anyhow::Error::msg)?,
                                ))
                            }
                            Request::Forget { slug, .. } => {
                                handle
                                    .request_typed::<OpenRouterModelForgetResponse>(
                                        ClientRequest::OpenRouterModelForget {
                                            request_id,
                                            params: OpenRouterModelForgetParams {
                                                slug: slug.clone(),
                                            },
                                        },
                                    )
                                    .await?;
                                Ok(Reply::Done)
                            }
                            Request::Endpoints { slug, .. } => {
                                let response = handle
                                    .request_typed::<OpenRouterModelEndpointsResponse>(
                                        ClientRequest::OpenRouterModelEndpoints {
                                            request_id,
                                            params: OpenRouterModelEndpointsParams {
                                                slug: slug.clone(),
                                            },
                                        },
                                    )
                                    .await?;
                                Ok(Reply::Endpoints(response))
                            }
                            Request::Route { slug, filter, .. } => {
                                handle
                                    .request_typed::<OpenRouterModelRouteResponse>(
                                        ClientRequest::OpenRouterModelRoute {
                                            request_id,
                                            params: OpenRouterModelRouteParams {
                                                slug: slug.clone(),
                                                filter: filter.clone(),
                                            },
                                        },
                                    )
                                    .await?;
                                Ok(Reply::Done)
                            }
                            Request::AddKey { name, path, .. } => {
                                handle
                                    .request_typed::<OpenRouterKeyAddResponse>(
                                        ClientRequest::OpenRouterKeyAdd {
                                            request_id,
                                            params: OpenRouterKeyAddParams {
                                                name: name.clone(),
                                                path: path.clone(),
                                            },
                                        },
                                    )
                                    .await?;
                                Ok(Reply::Done)
                            }
                            Request::RemoveKey { name, .. } => {
                                handle
                                    .request_typed::<OpenRouterKeyRemoveResponse>(
                                        ClientRequest::OpenRouterKeyRemove {
                                            request_id,
                                            params: OpenRouterKeyRemoveParams {
                                                name: name.clone(),
                                            },
                                        },
                                    )
                                    .await?;
                                Ok(Reply::Done)
                            }
                        }
                    })
                    .await
                    .map_err(|_| {
                        "OpenRouter request timed out; reopen the menu to check before retrying"
                            .to_owned()
                    })
                    .and_then(|result: anyhow::Result<Reply>| {
                        result.map_err(|error| crate::openrouter::error_message(&error))
                    });
                    tx.send(AppEvent::OpenRouter(Event::Finished {
                        id,
                        thread_id,
                        request,
                        result,
                    }));
                });
            }
            Event::Finished {
                id,
                thread_id,
                request,
                result,
            } => {
                let visible = self.chat_widget.finish_openrouter_request(id);
                if thread_id != self.current_displayed_thread_id() || !visible {
                    return;
                }
                match (request, result) {
                    (Request::Models { menu, selected }, Ok(Reply::Models(listing))) => self
                        .chat_widget
                        .show_openrouter_models(menu, listing, selected.as_ref(), None),
                    (Request::Endpoints { menu, slug }, Ok(Reply::Endpoints(response))) => self
                        .chat_widget
                        .show_openrouter_endpoints(menu, slug, response),
                    (Request::Save { menu, .. }, Ok(Reply::Saved(selected))) => {
                        self.handle_openrouter_event(
                            server,
                            Event::Request(Request::Models {
                                menu,
                                selected: Some(selected),
                            }),
                        );
                    }
                    (Request::Route { menu, slug, .. }, Ok(Reply::Done)) => {
                        self.handle_openrouter_event(
                            server,
                            Event::Request(Request::Models {
                                menu,
                                selected: Some(slug),
                            }),
                        );
                    }
                    (Request::Forget { menu, .. }, Ok(Reply::Done)) => {
                        self.handle_openrouter_event(
                            server,
                            Event::Request(Request::Models {
                                menu,
                                selected: None,
                            }),
                        );
                    }
                    (
                        Request::AddKey { thread_id, .. } | Request::RemoveKey { thread_id, .. },
                        Ok(Reply::Done),
                    ) => {
                        self.open_account_picker(server, thread_id);
                        self.chat_widget
                            .refresh_openrouter_status(StatusRefresh::Now);
                    }
                    (request, Err(error)) => {
                        self.chat_widget.show_openrouter_failure(request, error);
                    }
                    (_, Ok(_)) => self
                        .chat_widget
                        .show_openrouter_error("Unexpected OpenRouter response".into()),
                }
            }
            Event::RefreshStatus {
                thread_id,
                id,
                mode,
            } => {
                let handle = server.request_handle();
                let request_id = server.next_request_id();
                let status_request_id = server.next_request_id();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let result = tokio::time::timeout(Duration::from_secs(30), async {
                        let accounts = handle
                            .request_typed::<ThreadAccountListResponse>(
                                ClientRequest::ThreadAccountList {
                                    request_id,
                                    params: ThreadAccountListParams {
                                        thread_id: thread_id.to_string(),
                                    },
                                },
                            )
                            .await
                            .map_err(|error| {
                                StatusFailure::Failed(crate::openrouter::error_message(
                                    &error.into(),
                                ))
                            })?;
                        let name = accounts.selected_openrouter_key.ok_or_else(|| {
                            StatusFailure::Failed("Choose an OpenRouter key with /account".into())
                        })?;
                        if !accounts
                            .openrouter_keys
                            .iter()
                            .any(|status| status.key.name == name)
                        {
                            return Err(StatusFailure::Removed(name));
                        }
                        let response = handle
                            .request_typed::<OpenRouterKeyStatusResponse>(
                                ClientRequest::OpenRouterKeyStatus {
                                    request_id: status_request_id,
                                    params: OpenRouterKeyStatusParams {
                                        name: Some(name.clone()),
                                    },
                                },
                            )
                            .await
                            .map_err(|error| {
                                StatusFailure::Failed(crate::openrouter::error_message(
                                    &error.into(),
                                ))
                            })?;
                        response
                            .keys
                            .into_iter()
                            .find(|status| status.key.name == name)
                            .ok_or(StatusFailure::Removed(name))
                    })
                    .await
                    .unwrap_or_else(|_| {
                        Err(StatusFailure::Failed(
                            "OpenRouter key status timed out".into(),
                        ))
                    });
                    tx.send(AppEvent::OpenRouter(Event::StatusLoaded {
                        thread_id,
                        id,
                        mode,
                        result,
                    }));
                });
            }
            Event::StatusLoaded {
                thread_id,
                id,
                mode,
                result,
            } => {
                if self.current_displayed_thread_id() == Some(thread_id) {
                    self.chat_widget
                        .finish_openrouter_status(thread_id, id, mode, &result);
                }
            }
        }
    }
}
