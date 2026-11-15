use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::ThreadEventAttachment;
use super::ThreadId;
use super::tui;
use crate::app_server_session::ResumeModelSettings;
use crate::app_server_session::session_preparation::PreparedSession;
use crate::app_server_session::session_preparation::RequestPolicy;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::CodexErrorInfo;
use codex_protocol::sign_in_required::AccountProvider;
use codex_protocol::sign_in_required::SignInRequired;
use std::collections::HashMap;
use tokio_util::task::AbortOnDropHandle;
use uuid::Uuid;

pub(crate) fn message(required: &SignInRequired) -> String {
    format!(
        "{} sign-in expired for {}. Sign in again with /account; this conversation reconnects when sign-in completes.",
        required.provider_name(),
        required.account,
    )
}

/// TELOS(recoverable-sign-in): recover only confirmed auth failures, never uncertain requests.
pub(crate) fn from_report(error: &color_eyre::Report) -> Option<SignInRequired> {
    let TypedRequestError::Server { source, .. } = error.downcast_ref::<TypedRequestError>()?
    else {
        return None;
    };
    match serde_json::from_value(source.data.as_ref()?.get("codexErrorInfo")?.clone()).ok()? {
        CodexErrorInfo::SignInRequired(required) => Some(required),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct Recovery {
    tasks: HashMap<ThreadId, (Uuid, AbortOnDropHandle<()>)>,
}

pub(crate) enum RecoveryEvent {
    Retry(AccountProvider),
    Completed {
        id: ThreadId,
        request: Uuid,
        prepared: Box<PreparedSession>,
        handle: codex_app_server_client::AppServerRequestHandle,
    },
}

impl App {
    pub(super) async fn handle_sign_in_recovery(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        event: RecoveryEvent,
    ) -> color_eyre::Result<()> {
        match event {
            RecoveryEvent::Retry(provider) => {
                if self.chat_widget.session_draft
                    == Some(crate::chatwidget::SessionDraft::AwaitingSignIn(provider))
                {
                    self.chat_widget.restore_session_draft();
                    self.chat_widget.request_draft_session();
                }
                let ids = self.thread_event_channels.iter().filter_map(|(id, channel)| {
                    matches!(channel.attachment(), ThreadEventAttachment::SignInRequired(required)
                        if required.provider == provider).then_some(*id)
                }).collect::<Vec<_>>();
                for id in ids {
                    let plan = server.prepare_resume_thread(
                        RequestPolicy::Interactive,
                        &self.local_settings,
                        self.config.clone(),
                        id,
                        ResumeModelSettings::PreserveExistingThread,
                        crate::resume_permissions::ResumePermissions::default(),
                    );
                    let tx = self.app_event_tx.clone();
                    let request = Uuid::new_v4();
                    let handle = server.request_handle();
                    let task = AbortOnDropHandle::new(tokio::spawn(async move {
                        let prepared = Box::pin(plan.run()).await;
                        tx.send(AppEvent::SignInRecovery(RecoveryEvent::Completed {
                            id,
                            request,
                            prepared: Box::new(prepared),
                            handle,
                        }));
                    }));
                    self.reconnect
                        .sign_in_recovery
                        .tasks
                        .insert(id, (request, task));
                }
            }
            RecoveryEvent::Completed {
                id,
                request,
                prepared,
                handle,
            } => {
                if !server.accepts_session_observations(&prepared.observations) {
                    use crate::app_server_session::session_preparation::SubscriptionCustody;
                    let target = match prepared.custody {
                        SubscriptionCustody::Subscribed(target)
                        | SubscriptionCustody::Uncertain(target) => Some(target),
                        _ => None,
                    };
                    if let Some(target) = target {
                        self.request_server(server,
                            crate::app::turn_submissions::ui_requests::UiRequestScope::Connection,
                            crate::app::turn_submissions::ui_requests::UiRequestKind::Mutation,
                            "Release foreign sign-in recovery subscription",
                            move |_| async move {
                                crate::app::turn_submissions::detach::unsubscribe(&handle, Uuid::new_v4(), target).await
                            },
                            |app, _, result| {
                                if let Err(error) = result {
                                    app.chat_widget.add_error_message(format!("Foreign recovery cleanup was not confirmed: {error}. Reopen the original backend to resynchronize."));
                                }
                                Ok(())
                            });
                    }
                    return Ok(());
                }
                if self
                    .reconnect
                    .sign_in_recovery
                    .tasks
                    .get(&id)
                    .is_none_or(|(current, _)| *current != request)
                {
                    return Ok(());
                }
                self.reconnect.sign_in_recovery.tasks.remove(&id);
                let PreparedSession {
                    result,
                    observations,
                    ..
                } = *prepared;
                server.apply_session_observations(observations)?;
                match result {
                    Ok(started) => {
                        let displayed = self.current_displayed_thread_id() == Some(id);
                        self.install_selected_snapshot(id, started.session, started.turns, true)
                            .await;
                        if displayed {
                            self.active_thread_id = None;
                            self.select_agent_thread(tui, server, id).await?;
                        }
                    }
                    Err(error) => {
                        if let Some(required) = from_report(&error) {
                            self.ensure_thread_channel(id)
                                .require_sign_in(required.clone());
                            if self.current_displayed_thread_id() == Some(id) {
                                self.chat_widget.add_error_message(message(&required));
                            }
                        } else if self.current_displayed_thread_id() == Some(id) {
                            self.chat_widget.add_error_message(format!(
                                "Could not reconnect this conversation after sign-in: {error}"
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for RecoveryEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retry(provider) => f.debug_tuple("Retry").field(provider).finish(),
            Self::Completed { id, request, .. } => f
                .debug_struct("Completed")
                .field("id", id)
                .field("request", request)
                .finish_non_exhaustive(),
        }
    }
}
