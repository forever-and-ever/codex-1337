//! Credential-owning quota projection for status bars; never changes account selection.

use std::sync::Arc;
use std::time::Duration;

use codex_backend_client::Client;
use codex_core::config::Config;
use codex_login::AuthManager;
use codex_utils_cli::CliConfigOverrides;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountQuota {
    id: String,
    email: Option<String>,
    remaining_percent: Option<f64>,
    resets_at: Option<i64>,
    error: Option<String>,
}

pub async fn read_account_quotas(overrides: CliConfigOverrides) -> anyhow::Result<()> {
    let overrides = overrides.parse_overrides().map_err(anyhow::Error::msg)?;
    let config = Arc::new(Config::load_with_cli_overrides(overrides).await?);
    let auth =
        AuthManager::shared_from_config(config.as_ref(), /*enable_codex_api_key_env*/ false)
            .await?;
    let permits = Arc::new(tokio::sync::Semaphore::new(/*permits*/ 4));
    let mut jobs = tokio::task::JoinSet::new();
    for account in auth.saved_accounts()? {
        let auth = auth.clone();
        let config = config.clone();
        let permits = permits.clone();
        jobs.spawn(async move {
            let mut row = AccountQuota {
                id: account.id,
                email: account.email,
                remaining_percent: None,
                resets_at: None,
                error: None,
            };
            let result = async {
                let _permit = permits.acquire().await?;
                let scoped = auth.for_account(&row.id).await?;
                let auth = scoped
                    .auth()
                    .await
                    .ok_or_else(|| anyhow::anyhow!("account unavailable"))?;
                let client = Client::from_auth(
                    config.chatgpt_base_url.clone(),
                    &auth,
                    config.http_client_factory(),
                );
                let snapshots = client.get_rate_limits_many().await?;
                let snapshot = snapshots
                    .iter()
                    .find(|s| s.limit_id.as_deref() == Some("codex"))
                    .or_else(|| snapshots.first())
                    .ok_or_else(|| anyhow::anyhow!("no quota returned"))?;
                let weekly = snapshot
                    .primary
                    .iter()
                    .chain(snapshot.secondary.iter())
                    .find(|w| w.window_minutes == Some(10080))
                    .ok_or_else(|| anyhow::anyhow!("no weekly quota returned"))?;
                anyhow::ensure!(weekly.used_percent.is_finite(), "invalid weekly quota");
                Ok::<_, anyhow::Error>((
                    (100.0 - weekly.used_percent).clamp(0.0, 100.0),
                    weekly.resets_at,
                ))
            };
            match tokio::time::timeout(Duration::from_secs(5), result).await {
                Ok(Ok((remaining, resets_at))) => {
                    row.remaining_percent = Some(remaining);
                    row.resets_at = resets_at;
                }
                Ok(Err(error)) => row.error = Some(error.to_string()),
                Err(_) => row.error = Some("quota request timed out".into()),
            }
            row
        });
    }
    let mut accounts = Vec::new();
    while let Some(row) = jobs.join_next().await {
        accounts.push(row?);
    }
    accounts.sort_by(|a, b| a.email.cmp(&b.email).then_with(|| a.id.cmp(&b.id)));
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({"accounts": accounts}))?
    );
    Ok(())
}
