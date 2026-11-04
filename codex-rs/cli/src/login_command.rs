use super::LoginCommand;
use super::LoginSubcommand;
use crate::read_access_token_from_stdin;
use crate::read_api_key_from_stdin;
use crate::run_claude_account_command;
use crate::run_login_status;
use crate::run_login_with_access_token;
use crate::run_login_with_api_key;
use crate::run_login_with_browser;
use crate::run_login_with_claude_subscription;
use crate::run_login_with_device_code;
use std::io::BufRead;
use std::io::IsTerminal;
use std::io::Write;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(super) enum LoginProvider {
    #[value(name = "openai")]
    Openai,
    #[value(name = "claude_subscription")]
    ClaudeSubscription,
}

// TELOS(provider-login): never guess a provider for an unattended bare login.
fn choose_provider() -> std::io::Result<LoginProvider> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(std::io::Error::other(
            "Choose a sign-in provider with --provider openai|claude_subscription; bare codex login requires an interactive terminal.",
        ));
    }
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    writeln!(output, "Which provider would you like to sign in to?")?;
    writeln!(output, "  1. ChatGPT (OpenAI)")?;
    writeln!(output, "  2. Claude subscription (Anthropic)")?;
    loop {
        write!(output, "Provider [1-2]: ")?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            return Err(std::io::Error::other(
                "Sign-in cancelled: no provider selected.",
            ));
        }
        match answer.trim() {
            "1" => return Ok(LoginProvider::Openai),
            "2" => return Ok(LoginProvider::ClaudeSubscription),
            _ => writeln!(output, "Enter 1 for ChatGPT or 2 for Claude subscription.")?,
        }
    }
}

pub(super) async fn run(command: LoginCommand) -> ! {
    match command.action {
        Some(LoginSubcommand::Status) => match command.provider {
            Some(LoginProvider::ClaudeSubscription) => {
                Box::pin(run_claude_account_command(
                    command.config_overrides,
                    LoginSubcommand::Status,
                ))
                .await
            }
            Some(LoginProvider::Openai) => {
                Box::pin(run_login_status(command.config_overrides)).await
            }
            None => {
                Box::pin(crate::commands::login::provider_status::run_all(
                    command.config_overrides,
                ))
                .await
            }
        },
        Some(action @ (LoginSubcommand::Select { .. } | LoginSubcommand::Rename { .. })) => {
            if !matches!(command.provider, Some(LoginProvider::ClaudeSubscription)) {
                eprintln!(
                    "Use --provider claude_subscription to select or rename a Claude account."
                );
                std::process::exit(1);
            }
            Box::pin(run_claude_account_command(command.config_overrides, action)).await
        }
        None => {
            let provider = command.provider.unwrap_or_else(|| {
                if command.with_api_key
                    || command.with_access_token
                    || command.api_key.is_some()
                    || command.use_device_code
                    || command.issuer_base_url.is_some()
                    || command.client_id.is_some()
                {
                    return LoginProvider::Openai;
                }
                choose_provider().unwrap_or_else(|error| {
                    eprintln!("{error}");
                    std::process::exit(1);
                })
            });
            if matches!(provider, LoginProvider::ClaudeSubscription) {
                Box::pin(run_login_with_claude_subscription(command.config_overrides)).await;
            }
            if command.with_api_key && command.with_access_token {
                eprintln!(
                    "Choose one login credential source: --with-api-key or --with-access-token."
                );
                std::process::exit(1);
            }
            if command.use_device_code {
                Box::pin(run_login_with_device_code(
                    command.config_overrides,
                    command.issuer_base_url,
                    command.client_id,
                ))
                .await;
            }
            if command.api_key.is_some() {
                eprintln!(
                    "The --api-key flag is no longer supported. Pipe the key instead, e.g. `printenv OPENAI_API_KEY | codex login --with-api-key`."
                );
                std::process::exit(1);
            }
            if command.with_api_key {
                let key = read_api_key_from_stdin();
                Box::pin(run_login_with_api_key(command.config_overrides, key)).await;
            }
            if command.with_access_token {
                let token = read_access_token_from_stdin();
                Box::pin(run_login_with_access_token(command.config_overrides, token)).await;
            }
            Box::pin(run_login_with_browser(command.config_overrides)).await
        }
    }
}
