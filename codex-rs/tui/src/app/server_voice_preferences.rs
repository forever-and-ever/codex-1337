//! Connection-owned voice preference reads and acknowledged-write confirmation.

use super::*;
use codex_protocol::protocol::RealtimeVoice;
use codex_protocol::protocol::RealtimeVoicesList;

#[derive(Clone)]
pub(in crate::app) enum VoicePreferenceOperation {
    Open {
        cwd: PathBuf,
    },
    Save {
        cwd: PathBuf,
        voice: RealtimeVoice,
    },
    Confirm {
        cwd: PathBuf,
        voice: RealtimeVoice,
        write: api::ConfigWriteResponse,
    },
}

impl VoicePreferenceOperation {
    pub(super) fn is_read_only(&self) -> bool {
        matches!(self, Self::Open { .. } | Self::Confirm { .. })
    }
}

#[derive(Debug)]
pub(in crate::app) enum VoicePreferenceReceipt {
    Choices(Result<(RealtimeVoicesList, Option<RealtimeVoice>), String>),
    Saved(api::ConfigWriteResponse),
    Confirmed(Result<Option<RealtimeVoice>, String>),
}

async fn list_voices(
    handle: &AppServerRequestHandle,
    id: Uuid,
) -> Result<RealtimeVoicesList, String> {
    handle
        .request_typed::<api::ThreadRealtimeListVoicesResponse>(
            api::ClientRequest::ThreadRealtimeListVoices {
                request_id: api::RequestId::String(format!("tui-voice-list-{id}")),
                params: api::ThreadRealtimeListVoicesParams {},
            },
        )
        .await
        .map(|response| response.voices)
        .map_err(|error| error.to_string())
}

pub(super) async fn send(
    handle: AppServerRequestHandle,
    id: Uuid,
    operation: VoicePreferenceOperation,
) -> Result<OperationReceipt, TypedRequestError> {
    let receipt = match operation {
        VoicePreferenceOperation::Open { cwd } => {
            let result = async {
                let voices = list_voices(&handle, id)
                    .await
                    .map_err(|error| format!("Failed to list voices: {error}"))?;
                let config = crate::config_update::read_effective_config_if_supported(handle, &cwd)
                    .await
                    .map_err(|error| format!("Failed to read voice settings: {error}"))?;
                let selected = config
                    .as_ref()
                    .map(|config| {
                        super::super::realtime_settings::configured_voice(
                            &config.config,
                            voices.default_v1,
                        )
                    })
                    .transpose()
                    .map(Option::flatten)
                    .map_err(|error| format!("Failed to read voice settings: {error}"))?;
                Ok((voices, selected))
            }
            .await;
            VoicePreferenceReceipt::Choices(result)
        }
        VoicePreferenceOperation::Save { voice, .. } => {
            let response = handle
                .request_typed::<api::ConfigWriteResponse>(api::ClientRequest::ConfigBatchWrite {
                    request_id: api::RequestId::String(format!("tui-voice-save-{id}")),
                    params: api::ConfigBatchWriteParams {
                        edits: vec![crate::config_update::replace_config_value(
                            "realtime.voice",
                            serde_json::json!(voice),
                        )],
                        file_path: None,
                        expected_version: None,
                        reload_user_config: true,
                    },
                })
                .await?;
            VoicePreferenceReceipt::Saved(response)
        }
        VoicePreferenceOperation::Confirm { cwd, voice, write } => {
            let result = async {
                let voices = list_voices(&handle, id).await.map_err(|error| format!("Voice preference was saved, but the effective voice could not be confirmed: {error}"))?;
                let config = crate::config_update::read_effective_config_if_supported(handle, &cwd).await.map_err(|error| format!("Voice preference was saved, but effective settings could not be read: {error}"))?;
                match config {
                    Some(config) => super::super::realtime_settings::configured_voice(&config.config, voices.default_v1).map_err(|error| format!("Voice preference was saved, but effective settings could not be read: {error}")),
                    None if write.status == api::WriteStatus::OkOverridden => Err("Voice preference was saved, but effective settings could not be read: the saved voice is overridden, but this server cannot read effective settings".into()),
                    None => Ok(Some(voice)),
                }
            }.await;
            VoicePreferenceReceipt::Confirmed(result)
        }
    };
    Ok(OperationReceipt::VoicePreference(receipt))
}

impl App {
    pub(super) fn continue_voice_preference(
        &mut self,
        active: &mut PendingSubmission,
        write: &api::ConfigWriteResponse,
    ) -> bool {
        let SubmissionRequest::Operation(operation) = &mut active.request else {
            return false;
        };
        let ThreadOperation::Connection(ConnectionMutation::Voice(preference)) = &mut **operation
        else {
            return false;
        };
        let VoicePreferenceOperation::Save { cwd, voice } = preference else {
            return false;
        };
        *preference = VoicePreferenceOperation::Confirm {
            cwd: cwd.clone(),
            voice: *voice,
            write: write.clone(),
        };
        true
    }

    pub(super) fn finish_voice_preference(
        &mut self,
        operation: &VoicePreferenceOperation,
        receipt: VoicePreferenceReceipt,
    ) {
        match (operation, receipt) {
            (VoicePreferenceOperation::Open { cwd }, VoicePreferenceReceipt::Choices(result))
                if self.chat_widget.config_ref().cwd.as_path() == cwd =>
            {
                match result {
                    Ok((voices, selected)) => {
                        self.chat_widget.open_realtime_voices(selected, voices)
                    }
                    Err(error) => self.chat_widget.add_error_message(error),
                }
            }
            (
                VoicePreferenceOperation::Confirm { cwd, voice, write },
                VoicePreferenceReceipt::Confirmed(result),
            ) if self.chat_widget.config_ref().cwd.as_path() == cwd => match result {
                Ok(effective) => {
                    self.config.realtime.voice = effective;
                    self.chat_widget.set_realtime_voice(effective);
                    if effective == Some(*voice) {
                        self.chat_widget.on_realtime_voice_saved(*voice);
                    } else {
                        self.chat_widget.add_error_message(format!(
                            "Voice preference was saved but not applied: {}",
                            super::super::config_persistence::overridden_write_message(write)
                        ));
                    }
                }
                Err(error) => self.chat_widget.add_error_message(error),
            },
            (_, VoicePreferenceReceipt::Saved(_)) => {
                unreachable!("acknowledged preference write must advance to confirmation")
            }
            _ => {}
        }
    }
}
