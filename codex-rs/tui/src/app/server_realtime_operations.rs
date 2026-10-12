//! Realtime signaling carries the producing attempt through preparation and delivery.

use super::*;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;
use codex_protocol::protocol::RealtimeVoice;

#[derive(Clone, Copy, Debug)]
pub(in crate::app) struct RealtimeIdentity {
    pub(in crate::app) attempt_id: u64,
    pub(in crate::app) input_generation: u64,
}

#[derive(Clone)]
pub(in crate::app) enum RealtimeOperation {
    Prepare {
        identity: RealtimeIdentity,
        offer_sdp: String,
        model: Option<String>,
        cwd: PathBuf,
    },
    Start {
        identity: RealtimeIdentity,
        params: api::ThreadRealtimeStartParams,
    },
    Speech {
        identity: RealtimeIdentity,
        delivery_id: u64,
        text: String,
    },
}

impl RealtimeOperation {
    pub(super) fn identity(&self) -> RealtimeIdentity {
        match self {
            Self::Prepare { identity, .. }
            | Self::Start { identity, .. }
            | Self::Speech { identity, .. } => *identity,
        }
    }

    pub(super) fn is_preparation(&self) -> bool {
        matches!(self, Self::Prepare { .. })
    }

    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Prepare { .. } => "Preparing voice mode",
            Self::Start { .. } => "Starting voice mode",
            Self::Speech { .. } => "Delivering voice response",
        }
    }
}

pub(super) async fn send(
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    id: Uuid,
    operation: RealtimeOperation,
) -> Result<OperationReceipt, TypedRequestError> {
    let request_id = api::RequestId::String(format!("tui-operation-{id}"));
    match operation {
        RealtimeOperation::Prepare { cwd, .. } => {
            let result = async {
                let response: api::ThreadRealtimeListVoicesResponse = handle
                    .request_typed(api::ClientRequest::ThreadRealtimeListVoices {
                        request_id,
                        params: api::ThreadRealtimeListVoicesParams {},
                    })
                    .await?;
                let config =
                    crate::config_update::read_effective_config_if_supported(handle, &cwd).await?;
                config
                    .as_ref()
                    .map(|config| {
                        super::super::realtime_settings::configured_voice(
                            &config.config,
                            response.voices.default_v1,
                        )
                    })
                    .transpose()
                    .map(Option::flatten)
            }
            .await;
            Ok(OperationReceipt::RealtimePrepared(result.map_err(
                |error: color_eyre::eyre::Report| error.to_string(),
            )))
        }
        RealtimeOperation::Start { params, .. } => {
            let _: api::ThreadRealtimeStartResponse = handle
                .request_typed(api::ClientRequest::ThreadRealtimeStart { request_id, params })
                .await?;
            Ok(OperationReceipt::RealtimeStarted)
        }
        RealtimeOperation::Speech { text, .. } => {
            let _: api::ThreadRealtimeAppendSpeechResponse = handle
                .request_typed(api::ClientRequest::ThreadRealtimeAppendSpeech {
                    request_id,
                    params: api::ThreadRealtimeAppendSpeechParams {
                        thread_id: thread_id.to_string(),
                        text,
                    },
                })
                .await?;
            Ok(OperationReceipt::RealtimeSpeechAccepted)
        }
    }
}

fn start_params(
    thread_id: ThreadId,
    offer_sdp: String,
    model: Option<String>,
    voice: Option<RealtimeVoice>,
) -> api::ThreadRealtimeStartParams {
    api::ThreadRealtimeStartParams {
        thread_id: thread_id.to_string(),
        client_managed_handoffs: Some(true),
        delegation_ack_filler: None,
        flush_transcript_tail_on_session_end: None,
        codex_responses_as_items: None,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: None,
        backend_reasoning_status: false,
        codex_response_handoff_channel_prefixes: None,
        model,
        output_modality: codex_protocol::protocol::RealtimeOutputModality::Audio,
        include_startup_context: Some(false),
        initial_items: None,
        realtime_start_instructions: None,
        realtime_end_instructions: None,
        prompt: None,
        realtime_session_id: None,
        transport: Some(api::ThreadRealtimeStartTransport::Webrtc { sdp: offer_sdp }),
        version: Some(codex_protocol::protocol::RealtimeConversationVersion::V3),
        voice,
    }
}

impl App {
    pub(super) fn realtime_operation_is_current(
        &mut self,
        thread_id: ThreadId,
        operation: &RealtimeOperation,
    ) -> bool {
        let Some(widget) = self.voice_widget_for_thread(thread_id) else {
            return false;
        };
        let identity = operation.identity();
        match operation {
            RealtimeOperation::Prepare { .. } | RealtimeOperation::Start { .. } => widget
                .is_starting_realtime_attempt(
                    thread_id,
                    identity.attempt_id,
                    identity.input_generation,
                ),
            RealtimeOperation::Speech { delivery_id, .. } => {
                widget.has_pending_realtime_speech(*delivery_id)
                    && widget.is_current_realtime_attempt(
                        thread_id,
                        identity.attempt_id,
                        identity.input_generation,
                    )
            }
        }
    }

    pub(super) fn cancel_unsent_realtime_operation(
        &mut self,
        thread_id: ThreadId,
        operation: &RealtimeOperation,
    ) {
        let Some(widget) = self.voice_widget_for_thread(thread_id) else {
            return;
        };
        match operation {
            RealtimeOperation::Speech { delivery_id, .. } => {
                widget.restore_undelivered_realtime_speech(*delivery_id)
            }
            RealtimeOperation::Prepare { identity, .. }
            | RealtimeOperation::Start { identity, .. } => {
                widget.reject_unsent_realtime_start(
                    thread_id,
                    identity.attempt_id,
                    "Voice preparation canceled before signaling was sent.".into(),
                );
            }
        }
    }

    pub(super) fn continue_realtime_preparation(
        &mut self,
        thread_id: ThreadId,
        active: &mut PendingSubmission,
        result: Result<Option<RealtimeVoice>, String>,
    ) -> bool {
        let SubmissionRequest::Operation(operation) = &mut active.request else {
            return false;
        };
        let ServerOperation::Realtime(realtime) = &mut **operation else {
            return false;
        };
        if !self.realtime_operation_is_current(thread_id, realtime) {
            self.cancel_unsent_realtime_operation(thread_id, realtime);
            return false;
        }
        let RealtimeOperation::Prepare {
            identity,
            offer_sdp,
            model,
            ..
        } = realtime
        else {
            return false;
        };
        match result {
            Ok(voice) => {
                *realtime = RealtimeOperation::Start {
                    identity: *identity,
                    params: start_params(thread_id, offer_sdp.clone(), model.clone(), voice),
                };
                true
            }
            Err(error) => {
                if let Some(widget) = self.voice_widget_for_thread(thread_id) {
                    widget.reject_unsent_realtime_start(
                        thread_id,
                        identity.attempt_id,
                        format!("Voice conversation failed: {error}"),
                    );
                }
                false
            }
        }
    }

    pub(super) fn reject_realtime_operation(
        &mut self,
        thread_id: ThreadId,
        operation: &RealtimeOperation,
        failure: &SubmissionFailure,
    ) {
        let current = self.realtime_operation_is_current(thread_id, operation);
        let Some(widget) = self.voice_widget_for_thread(thread_id) else {
            return;
        };
        let mut message = match failure {
            SubmissionFailure::Rejected(error) => format!("{} failed: {error}", operation.label()),
            SubmissionFailure::Uncertain(error) => format!(
                "{} outcome is unknown: {error}. No retry was sent.",
                operation.label()
            ),
            SubmissionFailure::TimedOut => format!(
                "{} acknowledgement timed out; no retry was sent.",
                operation.label()
            ),
        };
        if !operation.is_preparation() && !matches!(failure, SubmissionFailure::Rejected(_)) {
            message.push_str(" Close and reopen this TUI to resynchronize voice status; the earlier request may still complete. A stop acknowledgement cannot settle the earlier unknown signal.");
        }
        match operation {
            RealtimeOperation::Speech { delivery_id, .. } => {
                if matches!(failure, SubmissionFailure::Rejected(_)) {
                    widget.restore_undelivered_realtime_speech(*delivery_id);
                } else {
                    widget.mark_realtime_speech_uncertain(*delivery_id);
                }
                if current && let SubmissionFailure::Rejected(error) = failure {
                    widget.on_realtime_error(format!(
                        "thread/realtime/appendSpeech failed in TUI: {error}"
                    ));
                } else {
                    widget.add_error_message(message);
                }
            }
            RealtimeOperation::Prepare { identity, .. } => {
                widget.reject_unsent_realtime_start(thread_id, identity.attempt_id, message)
            }
            RealtimeOperation::Start { identity, .. }
                if matches!(failure, SubmissionFailure::Rejected(_)) =>
            {
                let SubmissionFailure::Rejected(error) = failure else {
                    unreachable!()
                };
                widget.reject_unsent_realtime_start(
                    thread_id,
                    identity.attempt_id,
                    format!("thread/realtime/start failed in TUI: {error}"),
                );
            }
            // Do not reset a possibly-started session as though it were definitively unsent.
            RealtimeOperation::Start { .. } => widget.add_error_message(message),
        }
    }
}
