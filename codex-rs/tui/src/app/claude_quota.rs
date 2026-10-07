use super::App;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol::AccountQuotaReadParams;
use codex_app_server_protocol::AccountQuotaReadResponse;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::SubscriptionAccountProvider;

impl App {
    pub(super) fn refresh_claude_quota(
        &mut self,
        app_server: &mut AppServerSession,
        request_id: u64,
        account: Option<String>,
    ) {
        if self.reconnect.offline {
            self.app_event_tx.send(AppEvent::ClaudeQuotaLoaded {
                request_id,
                result: Err("app-server disconnected".to_owned()),
            });
            return;
        }
        let handle = app_server.request_handle();
        let rpc_request_id = app_server.next_request_id();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = handle
                .request_typed::<AccountQuotaReadResponse>(ClientRequest::AccountQuotaRead {
                    request_id: rpc_request_id,
                    params: AccountQuotaReadParams {
                        provider: SubscriptionAccountProvider::ClaudeSubscription,
                        account: account.clone(),
                    },
                })
                .await
                .map(|response| match account {
                    Some(email) => response
                        .accounts
                        .into_iter()
                        .find(|quota| quota.email.as_ref() == Some(&email)),
                    None if response.accounts.len() == 1 => response.accounts.into_iter().next(),
                    None => None,
                })
                .map_err(|error| error.to_string());
            tx.send(AppEvent::ClaudeQuotaLoaded { request_id, result });
        });
    }
}
