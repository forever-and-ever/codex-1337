use std::sync::Arc;

use codex_app_server_protocol::TurnToolOutput;
use codex_core::CodexThread;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::turn_input::TurnStartOptions;

use super::ThreadCoordinationService;

impl ThreadCoordinationService {
    pub(crate) async fn deliver_alarm(
        &self,
        thread: &Arc<CodexThread>,
        body: String,
    ) -> Result<TurnInputSubmission, String> {
        super::ensure_direct_input_allowed(thread)
            .await
            .map_err(|err| err.message)?;
        self.threads
            .config_manager
            .check_thread_model_provider(thread.config().await.as_ref())
            .await
            .map_err(|err| err.to_string())?;
        let sticky = thread
            .thread_extension_data()
            .get::<TurnStartOptions>()
            .map(|options| options.as_ref().clone())
            .unwrap_or_default();
        let item = TurnToolOutput {
            name: "set".into(),
            namespace: Some("alarm".into()),
            output: FunctionCallOutputBody::Text(body),
        }
        .into_core();
        thread
            .start_or_steer_turn(
                TurnInputRequest::new(TurnInput::ResponseItem(item)).on_start(TurnStartOptions {
                    turn_trigger: Some("alarm".into()),
                    ..sticky
                }),
            )
            .await
            .map_err(|err| err.to_string())
    }
}
