//! Thin quota reader. Credentials, refreshes and polling belong to the shared server.

use std::time::Duration;

use clap::Args;
use clap::ValueEnum;
use codex_app_server_client::RemoteAppServerClient;
use codex_app_server_client::RemoteAppServerConnectArgs;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_app_server_client::app_server_control_socket_path;
use codex_app_server_protocol::AccountProvider;
use codex_app_server_protocol::AccountQuotaReadParams;
use codex_app_server_protocol::AccountQuotaReadResponse;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_core::config::find_codex_home;
use codex_utils_cli::CliConfigOverrides;
use serde::Serialize;

#[derive(Debug, Args)]
pub struct QuotaCommand {
    /// Use the common five-hour/weekly JSON contract. Omit for legacy `OpenAI` weekly JSON.
    #[arg(long, value_enum)]
    provider: Option<QuotaProvider>,
    /// Saved account nickname, email or ID; otherwise read all accounts of this provider.
    #[arg(long, requires = "provider")]
    account: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum QuotaProvider {
    Openai,
    Anthropic,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LegacyAccountQuota {
    id: String,
    email: Option<String>,
    remaining_percent: Option<f64>,
    resets_at: Option<i64>,
    error: Option<String>,
}

pub async fn read_account_quotas(
    overrides: CliConfigOverrides,
    args: QuotaCommand,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        overrides.raw_overrides.is_empty(),
        "quota uses the shared server's configuration; -c overrides are not supported"
    );
    let socket_path = app_server_control_socket_path(find_codex_home()?.as_path())?;
    let response: AccountQuotaReadResponse =
        tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
            let client = RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
                required_human_approval: false,
                endpoint: RemoteAppServerEndpoint::UnixSocket { socket_path },
                client_name: "codex-quota".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                experimental_api: false,
                mcp_server_openai_form_elicitation: false,
                opt_out_notification_methods: Vec::new(),
                channel_capacity: 16,
            })
            .await?;
            let result = client
                .request_typed(ClientRequest::AccountQuotaRead {
                    request_id: RequestId::Integer(1),
                    params: AccountQuotaReadParams {
                        provider: match args.provider.unwrap_or(QuotaProvider::Openai) {
                            QuotaProvider::Openai => AccountProvider::Openai,
                            QuotaProvider::Anthropic => AccountProvider::ClaudeSubscription,
                        },
                        account: args.account,
                    },
                })
                .await;
            // A peer's slow close handshake must not consume the status bar's deadline.
            let _ = tokio::time::timeout(Duration::from_millis(/*millis*/ 200), client.shutdown())
                .await;
            Ok::<_, anyhow::Error>(result?)
        })
        .await
        .map_err(|_| anyhow::anyhow!("shared-server quota read timed out"))??;
    if args.provider.is_some() {
        println!("{}", serde_json::to_string(&response)?);
    } else {
        let accounts: Vec<_> = response
            .accounts
            .into_iter()
            .map(|row| LegacyAccountQuota {
                id: row.id,
                email: row.email,
                remaining_percent: row
                    .seven_day
                    .as_ref()
                    .map(|window| window.remaining_percent),
                resets_at: row.seven_day.as_ref().and_then(|window| window.resets_at),
                error: row.error.or_else(|| {
                    row.seven_day
                        .is_none()
                        .then(|| "no weekly quota returned".into())
                }),
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({"accounts": accounts}))?
        );
    }
    Ok(())
}
