use super::API_KEY_AUTHENTICATION_NOTICE;
use super::load_config_or_exit;
use super::safe_format_key;
use codex_core::config::Config;
use codex_login::AuthManager;
use codex_login::is_workload_identity_selected;
use codex_protocol::auth::AuthMode;
use codex_utils_cli::CliConfigOverrides;

// TELOS(provider-login): combined status observes both stores before choosing its exit status.
pub(crate) async fn run_all(overrides: CliConfigOverrides) -> ! {
    let config = load_config_or_exit(overrides).await;
    eprintln!("OpenAI (ChatGPT):");
    let openai = print_openai(&config).await;
    eprintln!("Claude subscription (Anthropic):");
    let claude = crate::commands::claude_login::print_login_status(&config);
    let code = match (openai, claude) {
        (Ok(openai), Ok(claude)) => i32::from(!openai && !claude),
        (openai, claude) => {
            if let Err(error) = openai {
                eprintln!("Error checking OpenAI login status: {error}");
            }
            if let Err(error) = claude {
                eprintln!("Error checking Claude login status: {error}");
            }
            1
        }
    };
    std::process::exit(code);
}

pub(super) async fn run_openai(overrides: CliConfigOverrides) -> ! {
    let config = load_config_or_exit(overrides).await;
    let code = match print_openai(&config).await {
        Ok(signed_in) => i32::from(!signed_in),
        Err(error) => {
            eprintln!("Error checking login status: {error}");
            1
        }
    };
    std::process::exit(code);
}

async fn print_openai(config: &Config) -> anyhow::Result<bool> {
    if is_workload_identity_selected() {
        AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false).await?;
        eprintln!("Logged in using workload identity");
        return Ok(true);
    }
    let Some(auth) = config
        .auth_config()
        .load_auth(/*enable_codex_api_key_env*/ false)
        .await?
    else {
        eprintln!("Not logged in");
        return Ok(false);
    };
    match auth.auth_mode() {
        AuthMode::ApiKey => {
            let key = auth.get_token()?;
            eprintln!("API key configured - {}", safe_format_key(&key));
            eprintln!("{API_KEY_AUTHENTICATION_NOTICE}");
        }
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => match auth.get_account_email() {
            Some(email) => eprintln!("Logged in using ChatGPT as {email}"),
            None => eprintln!("Logged in using ChatGPT"),
        },
        AuthMode::Headers => unreachable!("header auth cannot be loaded from auth storage"),
        AuthMode::AgentIdentity => eprintln!("Logged in using access token"),
        AuthMode::PersonalAccessToken => eprintln!("Logged in using personal access token"),
    }
    Ok(true)
}
