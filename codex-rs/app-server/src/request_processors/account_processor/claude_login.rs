use super::*;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_login::anthropic::AccountStore;
use codex_login::anthropic::LoginAttempt;

impl AccountRequestProcessor {
    pub(super) async fn login_claude_subscription(&self, request_id: ConnectionRequestId) {
        let result = LoginAttempt::start(&self.config.http_client_factory()).await;
        let attempt = match result {
            Ok(attempt) => attempt,
            Err(error) => {
                self.outgoing
                    .send_result::<LoginAccountResponse, JSONRPCErrorError>(
                        request_id,
                        Err(internal_error(error.to_string())),
                    )
                    .await;
                return;
            }
        };
        let login_id = Uuid::new_v4();
        let auth_url = attempt.authorization_url().to_owned();
        {
            let mut active = self.active_login.lock().await;
            *active = Some(ActiveLogin::ClaudeSubscription {
                cancel: attempt.cancel_handle(),
                login_id,
            });
        }
        let owner = request_id.connection_id;
        // Deliver the identifier before completion can race the initiating client.
        self.outgoing
            .send_result::<LoginAccountResponse, JSONRPCErrorError>(
                request_id,
                Ok(LoginAccountResponse::ClaudeSubscription {
                    login_id: login_id.to_string(),
                    auth_url,
                }),
            )
            .await;
        let processor = self.clone();
        tokio::spawn(async move {
            let store = AccountStore::new(processor.config.codex_home.to_path_buf());
            let (success, error) = match attempt.complete(&store).await {
                Ok(id) => {
                    let saved = async {
                        let _lock = codex_config::lock_account_preferences(
                            processor.config.codex_home.as_path(),
                        )
                        .await?;
                        let config = processor.config_manager.load_non_project_config().await?;
                        // Logging in adds an account; only the first becomes the new-conversation default.
                        if config.wrangler.default_claude_account.is_some() {
                            return Ok(());
                        }
                        ConfigEditsBuilder::for_config(&config)
                            .with_edits([ConfigEdit::SetPath {
                                segments: vec!["wrangler".into(), "default_claude_account".into()],
                                value: toml_edit::value(id.to_string()),
                            }])
                            .apply()
                            .await
                    }
                    .await;
                    (true, saved.err().map(|error| format!("Claude account saved, but the new-conversation default could not be saved: {error}")))
                }
                Err(error) => (false, Some(error.to_string())),
            };
            processor
                .outgoing
                .send_server_notification_to_connections(
                    &[owner],
                    ServerNotification::AccountLoginCompleted(AccountLoginCompletedNotification {
                        login_id: Some(login_id.to_string()),
                        success,
                        error,
                        onboarding_entrypoint: None,
                    }),
                )
                .await;
            let mut active = processor.active_login.lock().await;
            if active.as_ref().map(ActiveLogin::login_id) == Some(login_id) {
                *active = None;
            }
        });
    }
}
