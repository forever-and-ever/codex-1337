use codex_config::lock_account_preferences;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_login::anthropic::AccountStore;
use codex_login::anthropic::LoginAttempt;
use codex_utils_cli::CliConfigOverrides;

#[derive(Debug, clap::Subcommand)]
pub enum LoginAction {
    /// Show login status.
    Status,
    /// Select a saved Claude account for new conversations.
    Select { account_id: String },
    /// Rename a saved Claude account; an empty nickname clears its name.
    Rename {
        account_id: String,
        nickname: String,
    },
}

pub async fn run_claude_account_command(overrides: CliConfigOverrides, action: LoginAction) -> ! {
    let reload_overrides = overrides.clone();
    let config = crate::login::load_config_or_exit(overrides).await;
    let result = async {
        let store = AccountStore::new(config.codex_home.to_path_buf());
        let accounts = store.list()?;
        let (id, nickname) = match action {
            LoginAction::Status => {
                if accounts.is_empty() {
                    println!("No saved Claude accounts.");
                }
                for account in accounts {
                    let id = account.identity.id().to_string();
                    let selected = config.wrangler.default_claude_account.as_ref() == Some(&id);
                    let label = config
                        .wrangler
                        .account_nicknames
                        .get("claude_subscription")
                        .and_then(|names| names.get(&id))
                        .cloned()
                        .or(account.email)
                        .unwrap_or_default();
                    let label = label
                        .chars()
                        .filter(|c| {
                            !c.is_control()
                                && !matches!(*c as u32, 0x202A..=0x202E | 0x2066..=0x2069)
                        })
                        .take(96)
                        .collect::<String>();
                    println!("{id} · {label}{}", if selected { " (default)" } else { "" });
                }
                return Ok(());
            }
            LoginAction::Select { account_id } => (account_id, None),
            LoginAction::Rename {
                account_id,
                nickname,
            } => (account_id, Some(nickname)),
        };
        let account_id: codex_login::anthropic::AccountId = id.parse()?;
        if !accounts
            .iter()
            .any(|account| account.identity.id() == account_id)
        {
            return Err(anyhow::anyhow!("Claude subscription account is not saved"));
        }
        let _lock = lock_account_preferences(config.codex_home.as_path()).await?;
        let edit = match nickname {
            None => ConfigEdit::SetPath {
                segments: vec!["wrangler".into(), "default_claude_account".into()],
                value: id.into(),
            },
            Some(nickname) => {
                let nickname = codex_config::account_nickname(&nickname)?;
                let fresh = codex_core::config::Config::load_with_cli_overrides(
                    reload_overrides
                        .parse_overrides()
                        .map_err(anyhow::Error::msg)?,
                )
                .await?;
                if !nickname.is_empty()
                    && fresh
                        .wrangler
                        .account_nicknames
                        .get("claude_subscription")
                        .is_some_and(|names| {
                            names.iter().any(|(other, name)| {
                                other != &id && name.to_lowercase() == nickname.to_lowercase()
                            })
                        })
                {
                    return Err(anyhow::anyhow!(
                        "Nickname already belongs to another Claude account"
                    ));
                }
                let path = vec![
                    "wrangler".into(),
                    "account_nicknames".into(),
                    "claude_subscription".into(),
                    id,
                ];
                if nickname.is_empty() {
                    ConfigEdit::ClearPath { segments: path }
                } else {
                    ConfigEdit::SetPath {
                        segments: path,
                        value: nickname.into(),
                    }
                }
            }
        };
        ConfigEditsBuilder::for_config(&config)
            .with_edits([edit])
            .apply()
            .await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        eprintln!("Cannot update Claude account: {error}");
        std::process::exit(1);
    }
    std::process::exit(0);
}

pub async fn run_login_with_claude_subscription(overrides: CliConfigOverrides) -> ! {
    let config = crate::login::load_config_or_exit(overrides).await;
    let result = async {
        let attempt = LoginAttempt::start(&config.http_client_factory()).await?;
        eprintln!(
            "If your browser did not open, sign in to Claude at this URL:\n\n{}\n",
            attempt.authorization_url()
        );
        if let Err(error) = webbrowser::open(attempt.authorization_url()) {
            eprintln!("Cannot open browser: {error}");
        }
        let cancel = attempt.cancel_handle();
        let store = AccountStore::new(config.codex_home.to_path_buf());
        let login = attempt.complete(&store);
        tokio::pin!(login);
        let id = tokio::select! {
            result = &mut login => result?,
            signal = tokio::signal::ctrl_c() => {
                signal?;
                cancel.cancel();
                login.await?
            }
        };
        eprintln!("Claude account saved: {id}");
        let saved = async {
            let _lock = lock_account_preferences(config.codex_home.as_path()).await?;
            // Logging in adds an account; only the first becomes the new-conversation default.
            if codex_config::read_account_preferences(config.codex_home.as_path())
                .await?
                .and_then(|preferences| preferences.default_claude_account)
                .is_some()
            {
                return Ok(false);
            }
            ConfigEditsBuilder::for_config(&config)
                .with_edits([ConfigEdit::SetPath {
                    segments: vec!["wrangler".into(), "default_claude_account".into()],
                    value: id.to_string().into(),
                }])
                .apply()
                .await
                .map(|()| true)
        }
        .await;
        match saved {
            Ok(true) => {}
            Ok(false) => eprintln!("Select it for new conversations with /account."),
            Err(error) => eprintln!(
                "Account saved, but the new-conversation default could not be saved: {error}"
            ),
        }
        Ok::<_, std::io::Error>(())
    }
    .await;
    match result {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("Claude sign-in failed: {error}");
            std::process::exit(1);
        }
    }
}
